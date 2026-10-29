//! `handoff_trace_scaffold` (wiki/260-vmodel-m2-design.md §4.7, M2-12,
//! FR-305): generates one verification-layer item per acceptance-criteria
//! bullet of a source (left-side) item — the AC's Given/When/Then (or
//! EARS/plain text) content becomes a 手順 (steps) / 期待結果 (expected
//! result) statement, linked back to its source via `- from: <id>#<AC
//! label>` and `- verifies: <id>`.
//!
//! Idempotent (§4.7): an AC that already has a scaffolded item anywhere in
//! the project (a `SubItem.from` equal to this AC's `<id>#<label>`) is
//! skipped, not regenerated. ID collisions (`AT-REQ-003-1` already in use)
//! are avoided by appending a single lowercase-letter suffix
//! (`AT-REQ-003-1a`, `...1b`, ...).
//!
//! Rendering goes through [`crate::storage::docs::layer_render`] (shared
//! with `handoff_trace_update`'s `upsert_item` op, M2-14); writing
//! the rendered text is delegated entirely to
//! [`super::docs::handle_doc_save`]'s existing `append_body` path (this
//! module never touches `_doc.<slug>.md`/`DocMetadata` directly), which
//! already runs the real parser + layer sync on the new body — this is how
//! §4.7's "描画 → 解析の往復で同じ項目になる" is actually exercised end to
//! end, not just asserted in a unit test.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::{json, Value};

use super::docs::handle_doc_save;
use super::HandlerContext;
use crate::storage::config::{push_config_warning, read_trace_config_lenient};
use crate::storage::docs::layer::LayerRegistry;
use crate::storage::docs::layer_parse::{default_prefix_table, parse_layer_body, ParsedAc};
use crate::storage::docs::layer_render::{render_item, ItemRenderAttrs, PLACEHOLDER};
use crate::storage::docs::{find_doc_by_id, read_all_docs, read_doc, read_doc_body, DocMetadata};
use crate::storage::runs;
use crate::trace::profile::{resolve_profile_by_name, resolve_project_profile};

const DEFAULT_LIMIT: u64 = 20;
const DEFAULT_HEADING_LEVEL: u8 = 3;

#[derive(Debug, Clone, Serialize)]
struct ScaffoldGenerated {
    id: String,
    from: String,
    title: String,
}

#[derive(Debug, Clone, Serialize)]
struct ScaffoldSkipped {
    ac: String,
    existing: String,
}

fn to_json(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

/// Resolves a document by either its file-naming `slug` or its stable `id`
/// (same convention duplicated at `task_checklist::resolve_doc_by_slug_or_id`
/// and, privately, `docs::resolve_doc` — kept as a small local duplicate
/// rather than made `pub` in `docs.rs`, which is out of this task's scope).
fn resolve_doc_by_slug_or_id(
    handoff: &std::path::Path,
    slug_or_id: &str,
) -> Result<Option<DocMetadata>> {
    if let Some(doc) = read_doc(handoff, slug_or_id)? {
        return Ok(Some(doc));
    }
    find_doc_by_id(handoff, slug_or_id)
}

/// Finds which document (by slug) currently holds a persisted `SubItem`
/// with this `stable_id`, and that document's `layer` — a corpus-wide but
/// cheap scan (persisted metadata only, no body re-read) over `docs`
/// (already loaded by the caller).
fn find_owning_doc(docs: &[DocMetadata], stable_id: &str) -> Option<(String, Option<String>)> {
    for doc in docs {
        let Some(v) = &doc.verification else { continue };
        let found = v.items.iter().any(|item| {
            item.sub_items
                .iter()
                .any(|s| s.stable_id.as_deref() == Some(stable_id))
        });
        if found {
            return Some((doc.slug.clone(), doc.layer.clone()));
        }
    }
    None
}

/// Every stable_id some item declares as its `parent` (SPEC-TREE-014).
fn composite_ids(docs: &[DocMetadata]) -> HashSet<String> {
    docs.iter()
        .filter_map(|d| d.verification.as_ref())
        .flat_map(|v| v.items.iter())
        .flat_map(|item| item.sub_items.iter())
        .flat_map(|sub| sub.tree_parents.iter().cloned())
        .collect()
}

/// The "nothing to scaffold" warning for a source item without acceptance
/// criteria; a composite (decomposition parent) gets a pointer to its leaves
/// instead, since only leaves and composites with integrated acceptance
/// criteria are scaffold sources (SPEC-TREE-014).
fn no_acceptance_warning(id: &str, composite_ids: &HashSet<String>) -> String {
    if composite_ids.contains(id) {
        format!(
            "item '{id}' is a composite (it has decomposition children) without integrated \
             acceptance criteria; nothing to scaffold — scaffold its leaves, or give it its own \
             acceptance criteria"
        )
    } else {
        format!("item '{id}' has no acceptance criteria; nothing to scaffold")
    }
}

/// Splits one acceptance-criteria bullet into `(steps, expected_result,
/// heading_title)` per §4.7: a `gwt` AC's Given/When clauses become the
/// steps and its Then clause becomes both the expected result and the
/// heading title (untruncated); an `ears`/`text` AC has no distinguishable
/// steps ([`PLACEHOLDER`] — a "fill in" marker), its full text is the expected
/// result, and the heading title is its first 40 characters.
///
/// Deliberately literal, not a paraphrase: §4.7's own worked example
/// (`AT-REQ-003-1`) shows more natural Japanese phrasing than a mechanical
/// clause split produces, but no algorithm for that paraphrasing is
/// specified — this implementation takes the AC's own words for each
/// clause verbatim (with the `Given`/`When`/`Then` keywords themselves
/// stripped), which is deterministic and testable and satisfies the
/// literal rule text ("Given / When を手順、Then を期待結果にする").
fn split_ac(ac: &ParsedAc) -> (String, String, String) {
    if ac.kind == "gwt" {
        let lower = ac.text.to_lowercase();
        if let (Some(gi), Some(wi), Some(ti)) =
            (lower.find("given"), lower.find("when"), lower.find("then"))
        {
            if gi < wi && wi < ti {
                let given_clause = ac.text[gi + 5..wi].trim();
                let when_clause = ac.text[wi + 4..ti].trim();
                let then_clause = ac.text[ti + 4..].trim().to_string();
                let steps = if given_clause.is_empty() {
                    when_clause.to_string()
                } else {
                    format!("{given_clause}. {when_clause}")
                };
                return (steps, then_clause.clone(), then_clause);
            }
        }
    }
    let expected = ac.text.trim().to_string();
    let title: String = expected.chars().take(40).collect();
    (PLACEHOLDER.to_string(), expected, title)
}

/// Renders the ISO 29119 test-specification body for one scaffolded item
/// (SPEC-VT-001): objective, preconditions, numbered steps, expected result
/// and the result-recording fields (actual result / verdict / evidence) that
/// a later run fills in. `steps` is [`split_ac`]'s first element — a
/// [`PLACEHOLDER`] (non-GWT AC) is shown as `(TBD)`.
fn scaffold_statement(source_id: &str, ac_label: &str, steps: &str, expected: &str) -> String {
    let todo = format!("({PLACEHOLDER})");
    let steps = if steps == PLACEHOLDER {
        todo.as_str()
    } else {
        steps
    };
    format!(
        "**検証目的:**\n{source_id} の受入基準 {ac_label} を検証する\n\n\
         **前提条件:**\n{todo}\n\n\
         **手順:**\n1. {steps}\n\n\
         **期待結果:**\n- {expected}\n\n\
         **実結果:** _(未実行)_\n\
         **判定:** pending\n\
         **エビデンス:** _(なし)_"
    )
}

/// Rule-based verification method for a scaffolded item (no LLM involved):
/// a source requirement that already links automated tests (`test_refs`) is
/// verified `auto`; everything else — including GUI-oriented requirements —
/// is `manual`.
fn scaffold_method(source_has_test_refs: bool) -> &'static str {
    if source_has_test_refs {
        "auto"
    } else {
        "manual"
    }
}

/// `handoff_trace_scaffold` (§4.7). Input: exactly one of `items: [id]` /
/// `doc: <slug-or-id>` (source items), `target_doc: <slug-or-id>`
/// (required — a layer document), `mode: "preview" | "apply"` (default
/// `"preview"`), `limit` (default 20, caps the number of *generated* items
/// per call — an already-skipped AC does not count against it).
pub fn handle_trace_scaffold(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let items_arg: Option<Vec<String>> =
        arguments.get("items").and_then(|v| v.as_array()).map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        });
    let doc_arg = arguments.get("doc").and_then(|v| v.as_str());
    if items_arg.is_some() && doc_arg.is_some() {
        bail!("'items' and 'doc' are mutually exclusive");
    }
    if items_arg.is_none() && doc_arg.is_none() {
        bail!("either 'items' or 'doc' is required");
    }

    let target_doc_arg = arguments
        .get("target_doc")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'target_doc' is required"))?;

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

    let (trace_config, config_warning) = read_trace_config_lenient(handoff);
    let registry = LayerRegistry::build(&trace_config.layer);
    let mut warnings: Vec<String> = registry.warnings.clone();
    push_config_warning(&mut warnings, config_warning);
    let prefix_table = default_prefix_table(&registry, &trace_config.id_prefixes);

    let target_doc = resolve_doc_by_slug_or_id(handoff, target_doc_arg)?
        .ok_or_else(|| anyhow::anyhow!("Document not found: {target_doc_arg}"))?;
    let target_layer = target_doc.layer.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "target_doc '{target_doc_arg}' is not a layer document (no 'layer' set) — \
             handoff_trace_scaffold writes verification-layer items and needs a layer to \
             derive their id prefix from"
        )
    })?;
    let target_prefix = registry
        .get(&target_layer)
        .and_then(|l| l.default_id_prefixes.first().cloned())
        .ok_or_else(|| {
            anyhow::anyhow!("no default id prefix is configured for layer '{target_layer}'")
        })?;

    let all_docs = read_all_docs(handoff)?;
    // SPEC-TREE-014: ids that are the `parent` of some item — a source item
    // among them is a composite, which only scaffolds when it carries its own
    // integrated acceptance criteria (otherwise its leaves do).
    let composite_ids = composite_ids(&all_docs);

    // Gather source items: each is (id, its parsed acceptance-criteria
    // bullets). The persisted `SubItem.acceptance` only holds `{label,
    // kind}` (D1 — no AC text), so the owning document's body must be
    // re-parsed to recover the actual bullet text `split_ac` needs. This is
    // scoped to only the document(s) that actually hold a requested source
    // item, not a corpus-wide re-parse.
    let mut source_items: Vec<(String, Vec<ParsedAc>)> = Vec::new();
    match (&items_arg, doc_arg) {
        (Some(ids), None) => {
            let mut body_cache: HashMap<
                String,
                Vec<crate::storage::docs::layer_parse::ParsedItem>,
            > = HashMap::new();
            for id in ids {
                let Some((owner_slug, doc_layer)) = find_owning_doc(&all_docs, id) else {
                    warnings.push(format!("item '{id}' not found in the project"));
                    continue;
                };
                let parsed_items = body_cache.entry(owner_slug.clone()).or_insert_with(|| {
                    // `or_insert_with`'s closure can't propagate a `Result`;
                    // an I/O error or missing body here degrades to "no
                    // items parsed", which the `match` below turns into the
                    // already-user-facing "could not be re-parsed" warning
                    // rather than aborting the whole call over one item.
                    let body = read_doc_body(handoff, &owner_slug)
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    parse_layer_body(&body, doc_layer.as_deref(), &prefix_table).items
                });
                match parsed_items.iter().find(|it| &it.id == id) {
                    Some(item) if !item.acceptance.is_empty() => {
                        source_items.push((item.id.clone(), item.acceptance.clone()));
                    }
                    Some(_) => warnings.push(no_acceptance_warning(id, &composite_ids)),
                    None => warnings.push(format!(
                        "item '{id}' could not be re-parsed from its document body (concurrent edit?)"
                    )),
                }
            }
        }
        (None, Some(doc_ref)) => {
            let src_doc = resolve_doc_by_slug_or_id(handoff, doc_ref)?
                .ok_or_else(|| anyhow::anyhow!("Document not found: {doc_ref}"))?;
            let body = read_doc_body(handoff, &src_doc.slug)?.unwrap_or_default();
            let parsed = parse_layer_body(&body, src_doc.layer.as_deref(), &prefix_table);
            for item in parsed.items {
                if !item.acceptance.is_empty() {
                    source_items.push((item.id, item.acceptance));
                } else if composite_ids.contains(&item.id) {
                    warnings.push(no_acceptance_warning(&item.id, &composite_ids));
                }
            }
        }
        _ => unreachable!("exactly one of items/doc is present, checked above"),
    }

    // Project-wide idempotency ((`from`, layer) -> existing stable_id) and id
    // collision set — both read straight off already-persisted `SubItem`
    // fields (no body re-read needed for either check). The key includes the
    // owning document's layer so one AC can be scaffolded once per
    // verification layer (acceptance, then system_test, ...).
    let mut existing_from: HashMap<(String, Option<String>), String> = HashMap::new();
    let mut existing_ids: HashSet<String> = HashSet::new();
    for doc in &all_docs {
        let Some(v) = &doc.verification else { continue };
        for item in &v.items {
            for sub in &item.sub_items {
                if let Some(sid) = &sub.stable_id {
                    existing_ids.insert(sid.clone());
                }
                if let (Some(sid), Some(from)) = (&sub.stable_id, &sub.from) {
                    existing_from
                        .entry((from.clone(), doc.layer.clone()))
                        .or_insert_with(|| sid.clone());
                }
            }
        }
    }

    // Per-source attributes the generated items derive from: whether the
    // source already has linked tests (-> `method`) and its priority
    // (inherited). Read off the persisted `SubItem`s, like the maps above.
    let mut source_meta: HashMap<String, (bool, Option<String>)> = HashMap::new();
    for doc in &all_docs {
        let Some(v) = &doc.verification else { continue };
        for item in &v.items {
            for sub in &item.sub_items {
                if let Some(sid) = &sub.stable_id {
                    source_meta
                        .entry(sid.clone())
                        .or_insert_with(|| (!sub.test_refs.is_empty(), sub.priority.clone()));
                }
            }
        }
    }

    // Heading level for the appended items: match the target document's
    // last existing item (visual consistency with what's already there), or
    // fall back to level 3 (§4.7's own worked example, and §2.2's other
    // examples, consistently use `###`).
    let target_body = read_doc_body(handoff, &target_doc.slug)?.unwrap_or_default();
    let target_parsed = parse_layer_body(&target_body, target_doc.layer.as_deref(), &prefix_table);
    let heading_level = target_parsed
        .items
        .last()
        .map(|it| it.heading_level)
        .unwrap_or(DEFAULT_HEADING_LEVEL);

    // `runs::sync` (not a read-only path — this tool is already
    // write-classified, so its `_latest.json` refresh side effect is not a
    // new E6 violation) resolves the implicit-acceptance-item warning below
    // (§4.7: "暗黙の受入検証項目があるプロファイル ... run がある AC を
    // warnings で知らせる").
    let latest = runs::sync(handoff)?;

    let mut generated: Vec<ScaffoldGenerated> = Vec::new();
    let mut skipped: Vec<ScaffoldSkipped> = Vec::new();
    let mut rendered_bodies: Vec<String> = Vec::new();
    let mut newly_used_ids: HashSet<String> = HashSet::new();

    'outer: for (source_id, acs) in &source_items {
        for ac in acs {
            let candidate_from = format!("{source_id}#{}", ac.label);
            if let Some(existing_id) =
                existing_from.get(&(candidate_from.clone(), Some(target_layer.clone())))
            {
                skipped.push(ScaffoldSkipped {
                    ac: candidate_from,
                    existing: existing_id.clone(),
                });
                continue;
            }
            if generated.len() >= limit {
                break 'outer;
            }

            let ac_number = ac.label.trim_start_matches("AC");
            let base_id = format!("{target_prefix}-{source_id}-{ac_number}");
            let candidate_id = if existing_ids.contains(&base_id)
                || newly_used_ids.contains(&base_id)
            {
                let mut found = None;
                for c in b'a'..=b'z' {
                    let suffixed = format!("{base_id}{}", c as char);
                    if !existing_ids.contains(&suffixed) && !newly_used_ids.contains(&suffixed) {
                        found = Some(suffixed);
                        break;
                    }
                }
                match found {
                    Some(id) => id,
                    None => {
                        warnings.push(format!(
                            "could not allocate an id for {candidate_from}: '{base_id}' and \
                             every lettered suffix (a-z) are already in use"
                        ));
                        continue;
                    }
                }
            } else {
                base_id
            };
            newly_used_ids.insert(candidate_id.clone());

            if let Some(latest_result) = latest.items.get(&candidate_from) {
                warnings.push(format!(
                    "implicit acceptance item '{candidate_from}' has a recorded run ({}); it \
                     will not be carried over to the new item '{candidate_id}'",
                    latest_result.result
                ));
            }

            let (steps, expected, title) = split_ac(ac);
            // A source found by `find_owning_doc`/doc-mode always has a
            // persisted SubItem, so the lookup only misses on a concurrent
            // edit; "no tests, no priority" (manual) is the safe fallback.
            let (has_tests, source_priority) =
                source_meta.get(source_id).cloned().unwrap_or((false, None));
            let attrs = ItemRenderAttrs {
                verifies: vec![source_id.clone()],
                priority: source_priority,
                from: Some(candidate_from.clone()),
                method: Some(scaffold_method(has_tests).to_string()),
                environment: Some(format!("({PLACEHOLDER})")),
                ..Default::default()
            };
            let statement = scaffold_statement(source_id, &ac.label, &steps, &expected);
            rendered_bodies.push(render_item(
                heading_level,
                &candidate_id,
                &title,
                &attrs,
                &statement,
            ));
            generated.push(ScaffoldGenerated {
                id: candidate_id,
                from: candidate_from,
                title,
            });
        }
    }

    let mut applied = false;
    if mode == "apply" && !rendered_bodies.is_empty() {
        let save_result = handle_doc_save(
            ctx,
            &json!({
                "doc_id": target_doc.id,
                "append_body": rendered_bodies.join("\n\n"),
            }),
        )?;
        // `handle_doc_save` only ever returns a `to_json`-produced string on
        // `Ok` (its own error path returns `Err`, not malformed JSON), so
        // this parse cannot fail in practice; `Value::Null` here is a
        // best-effort fallback for the *warnings passthrough* only — the
        // write itself already succeeded (the `?` above would have
        // propagated a real failure), so a `Value::Null` simply yields no
        // extra warnings to merge (`.get("warnings")` on `Null` is `None`)
        // rather than losing the successful write's own response.
        let save_json: Value = serde_json::from_str(&save_result).unwrap_or(Value::Null);
        if let Some(save_warnings) = save_json.get("warnings").and_then(|v| v.as_array()) {
            for w in save_warnings {
                if let Some(s) = w.as_str() {
                    warnings.push(s.to_string());
                }
            }
        }
        applied = true;
    }

    // NFR-006 (wiki/270 §2.7): an additional safety-net warning when the
    // actual apply-mode generated count exceeds the resolved profile's
    // `max_generated_per_call` — generation itself stays governed by `limit`
    // (already applied above); this never truncates `generated`. Resolved
    // from the target_doc's own `trace_profile` override when set (the
    // generated items are verification-layer items that live under
    // target_doc, so its profile is the relevant one), else the project
    // default profile — same priority `resolve_doc_implicit_acceptance`
    // (`storage::docs::layer_sync`) already uses for the same target_doc.
    if mode == "apply" {
        let max_generated_per_call = match target_doc
            .trace_profile
            .as_deref()
            .filter(|s| !s.is_empty())
        {
            Some(name) => {
                let (resolved, _) = resolve_profile_by_name(name, &trace_config, &registry);
                resolved.and_then(|p| p.max_generated_per_call)
            }
            None => {
                let (resolved, _) = resolve_project_profile(&trace_config, &registry);
                resolved.and_then(|p| p.max_generated_per_call)
            }
        };
        if let Some(max) = max_generated_per_call {
            let generated_count = generated.len() as u32;
            if generated_count > max {
                warnings.push(format!(
                    "Generated {generated_count} items, exceeding profile limit of {max}"
                ));
            }
        }
    }

    Ok(to_json(&json!({
        "target_doc": target_doc.slug,
        "mode": mode,
        "applied": applied,
        "generated": generated,
        "skipped": skipped,
        "warnings": warnings,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::DocMetadata;
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

    fn layer_doc(id: &str, slug: &str, layer: &str) -> DocMetadata {
        let mut doc = DocMetadata::new(
            id.to_string(),
            slug.to_string(),
            format!("Title {id}"),
            "spec".to_string(),
            "2026-09-28T00:00:00Z".to_string(),
        );
        doc.layer = Some(layer.to_string());
        doc
    }

    fn ac(label: &str, kind: &'static str, text: &str) -> ParsedAc {
        ParsedAc {
            label: label.to_string(),
            kind,
            text: text.to_string(),
            ac_hash: String::new(),
        }
    }

    #[test]
    fn split_ac_gwt_splits_given_when_into_steps_and_then_into_expected() {
        let (steps, expected, title) = split_ac(&ac(
            "AC1",
            "gwt",
            "Given the account already failed 4 times When it fails a 5th time Then the account is locked",
        ));
        assert_eq!(
            steps,
            "the account already failed 4 times. it fails a 5th time"
        );
        assert_eq!(expected, "the account is locked");
        assert_eq!(title, "the account is locked");
    }

    #[test]
    fn split_ac_ears_uses_full_text_as_expected_and_placeholder_steps() {
        let (steps, expected, title) = split_ac(&ac(
            "AC2",
            "ears",
            "WHEN the account is locked THE SYSTEM SHALL reject even a correct password",
        ));
        assert_eq!(steps, PLACEHOLDER);
        assert_eq!(
            expected,
            "WHEN the account is locked THE SYSTEM SHALL reject even a correct password"
        );
        assert_eq!(
            title,
            "WHEN the account is locked THE SYSTEM SHALL reje"
                .chars()
                .take(40)
                .collect::<String>()
        );
    }

    #[test]
    fn split_ac_text_truncates_title_to_40_chars() {
        let long_text = "x".repeat(80);
        let (steps, expected, title) = split_ac(&ac("AC1", "text", &long_text));
        assert_eq!(steps, PLACEHOLDER);
        assert_eq!(expected, long_text);
        assert_eq!(title.chars().count(), 40);
    }

    #[test]
    fn split_ac_text_truncation_is_char_safe_for_multibyte_text() {
        let long_text = "受入基準".repeat(20); // far more than 40 chars, all multibyte
        let (_, _, title) = split_ac(&ac("AC1", "text", &long_text));
        assert_eq!(title.chars().count(), 40);
    }

    #[test]
    fn scaffold_statement_has_iso_29119_sections_with_gwt_steps() {
        let st = scaffold_statement(
            "REQ-003",
            "AC1",
            "the account already failed 4 times. it fails a 5th time",
            "the account is locked",
        );
        assert_eq!(
            st,
            "**検証目的:**\nREQ-003 の受入基準 AC1 を検証する\n\n\
             **前提条件:**\n(TBD)\n\n\
             **手順:**\n1. the account already failed 4 times. it fails a 5th time\n\n\
             **期待結果:**\n- the account is locked\n\n\
             **実結果:** _(未実行)_\n\
             **判定:** pending\n\
             **エビデンス:** _(なし)_"
        );
    }

    #[test]
    fn scaffold_statement_uses_tbd_step_placeholder_for_non_gwt() {
        let st = scaffold_statement("REQ-003", "AC2", PLACEHOLDER, "WHEN x THE SYSTEM SHALL y");
        assert!(st.contains("**手順:**\n1. (TBD)\n"), "{st}");
        assert!(
            st.contains("**期待結果:**\n- WHEN x THE SYSTEM SHALL y\n"),
            "{st}"
        );
    }

    #[test]
    fn scaffold_method_is_auto_only_with_test_refs() {
        assert_eq!(scaffold_method(true), "auto");
        assert_eq!(scaffold_method(false), "manual");
    }

    /// SPEC-VT-001: new scaffolded items carry the ISO 29119 template, an
    /// `environment` placeholder, the source's priority, and a rule-based
    /// `method` (`auto` iff the source has test_refs); the template
    /// round-trips through the real parser/layer sync.
    #[test]
    fn handler_apply_uses_iso_29119_template_priority_and_rule_based_method() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let req_doc = layer_doc("doc-req", "req-doc", "requirement");
        crate::storage::docs::write_doc(&handoff, &req_doc).unwrap();
        handle_doc_save(
            &c,
            &json!({"doc_id": "doc-req", "body": "# Requirements\n\n\
                ### REQ-010 Tested\n\n- priority: P0\n- test: tests/a.rs::t\n\n本文。\n\n\
                受入基準:\n- AC1: Given a When b Then locked\n- AC2: WHEN x THE SYSTEM SHALL y\n\n\
                ### REQ-011 Untested\n\n- priority: P2\n\n本文。\n\n\
                受入基準:\n- AC1: WHEN x THE SYSTEM SHALL y\n"}),
        )
        .unwrap();
        let at_doc = layer_doc("doc-at", "at-doc", "acceptance");
        crate::storage::docs::write_doc(&handoff, &at_doc).unwrap();
        handle_doc_save(&c, &json!({"doc_id": "doc-at", "body": "# Acceptance\n"})).unwrap();

        handle_trace_scaffold(
            &c,
            &json!({"items": ["REQ-010", "REQ-011"], "target_doc": "at-doc", "mode": "apply"}),
        )
        .unwrap();

        let body = read_doc_body(&handoff, "at-doc").unwrap().unwrap();
        let prefix_table = default_prefix_table(&LayerRegistry::build(&[]), &Default::default());
        let parsed = parse_layer_body(&body, Some("acceptance"), &prefix_table);
        let by_id = |id: &str| parsed.items.iter().find(|i| i.id == id).unwrap();

        let a = by_id("AT-REQ-010-1");
        assert_eq!(a.attrs.method.as_deref(), Some("auto"));
        assert_eq!(a.attrs.priority.as_deref(), Some("P0"));
        assert_eq!(a.ext_attrs.environment.as_deref(), Some("(TBD)"));
        assert!(a
            .statement
            .contains("**検証目的:**\nREQ-010 の受入基準 AC1 を検証する"));
        assert!(a.statement.contains("**判定:** pending"));

        let b = by_id("AT-REQ-010-2");
        assert!(
            b.statement.contains("**手順:**\n1. (TBD)"),
            "{}",
            b.statement
        );

        let u = by_id("AT-REQ-011-1");
        assert_eq!(u.attrs.method.as_deref(), Some("manual"));
        assert_eq!(u.attrs.priority.as_deref(), Some("P2"));
    }

    #[test]
    fn handler_requires_exactly_one_of_items_or_doc() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let err = handle_trace_scaffold(
            &c,
            &json!({ "items": ["REQ-1"], "doc": "some-doc", "target_doc": "at" }),
        );
        assert!(err.is_err());

        let (_tmp2, handoff2) = setup();
        let c2 = ctx(handoff2);
        let err2 = handle_trace_scaffold(&c2, &json!({ "target_doc": "at" }));
        assert!(err2.is_err());
    }

    #[test]
    fn handler_errors_when_target_doc_is_not_a_layer_document() {
        let (_tmp, handoff) = setup();
        let mut doc = DocMetadata::new(
            "doc-1".to_string(),
            "notes".to_string(),
            "Notes".to_string(),
            "note".to_string(),
            "2026-09-28T00:00:00Z".to_string(),
        );
        doc.layer = None;
        crate::storage::docs::write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff);
        let err = handle_trace_scaffold(&c, &json!({ "items": ["REQ-1"], "target_doc": "notes" }));
        assert!(err.is_err());
    }

    /// End-to-end within the handler (no server process): a requirement doc
    /// with two ACs scaffolds two items into an acceptance-layer target_doc,
    /// and a second `apply` call is a no-op (idempotent).
    /// SPEC-TREE-014: only leaves and composites carrying integrated
    /// acceptance criteria are scaffold sources; a composite without them is
    /// pointed at its leaves.
    #[test]
    fn handler_scaffolds_leaves_and_integrated_composites_but_not_bare_composites() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let req_doc = layer_doc("doc-req", "req-doc", "requirement");
        crate::storage::docs::write_doc(&handoff, &req_doc).unwrap();
        handle_doc_save(
            &c,
            &json!({"doc_id": "doc-req", "body": "# Requirements\n\n\
                ### REQ-001 Bare composite\n\nNo criteria of its own.\n\n\
                ### REQ-002 Integrated composite\n\n受入基準:\n- AC1: WHEN all parts work THE SYSTEM SHALL cooperate\n\n\
                ### REQ-003 Leaf\n\n- parent: REQ-001, REQ-002\n\n受入基準:\n- AC1: WHEN leaf works THE SYSTEM SHALL pass\n"}),
        )
        .unwrap();
        let at_doc = layer_doc("doc-at", "at-doc", "acceptance");
        crate::storage::docs::write_doc(&handoff, &at_doc).unwrap();
        handle_doc_save(&c, &json!({"doc_id": "doc-at", "body": "# Acceptance\n"})).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_scaffold(
                &c,
                &json!({"doc": "req-doc", "target_doc": "at-doc", "mode": "preview"}),
            )
            .unwrap(),
        )
        .unwrap();
        let froms: Vec<&str> = out["generated"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["from"].as_str().unwrap())
            .collect();
        assert_eq!(froms, vec!["REQ-002#AC1", "REQ-003#AC1"], "{out}");
        let warnings = out["warnings"].to_string();
        assert!(
            warnings.contains("REQ-001") && warnings.contains("composite"),
            "{warnings}"
        );

        let items_out: Value = serde_json::from_str(
            &handle_trace_scaffold(
                &c,
                &json!({"items": ["REQ-001"], "target_doc": "at-doc", "mode": "preview"}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(
            items_out["generated"].as_array().unwrap().is_empty(),
            "{items_out}"
        );
        assert!(
            items_out["warnings"].to_string().contains("composite"),
            "{items_out}"
        );
    }

    #[test]
    fn handler_apply_generates_items_and_is_idempotent_on_repeat() {
        let (_tmp, handoff) = setup();
        let req_body = "# Requirements\n\n\
            ### REQ-003 Account lockout\n\n\
            Statement text.\n\n\
            受入基準:\n\
            - AC1: Given a When b Then locked\n\
            - AC2: WHEN locked THE SYSTEM SHALL reject login\n";
        let req_doc = layer_doc("doc-req", "req-doc", "requirement");
        crate::storage::docs::write_doc(&handoff, &req_doc).unwrap();
        // Sync the requirement doc for real via doc_save, so its persisted
        // SubItem/acceptance state matches what a real caller would see.
        let c = ctx(handoff.clone());
        handle_doc_save(&c, &json!({ "doc_id": "doc-req", "body": req_body })).unwrap();

        let at_doc = layer_doc("doc-at", "at-doc", "acceptance");
        crate::storage::docs::write_doc(&handoff, &at_doc).unwrap();
        handle_doc_save(&c, &json!({ "doc_id": "doc-at", "body": "# Acceptance\n" })).unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_scaffold(
                &c,
                &json!({ "items": ["REQ-003"], "target_doc": "at-doc", "mode": "apply" }),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["applied"], true, "{out}");
        let generated = out["generated"].as_array().unwrap();
        assert_eq!(generated.len(), 2, "{out}");
        assert_eq!(generated[0]["id"], "AT-REQ-003-1");
        assert_eq!(generated[1]["id"], "AT-REQ-003-2");

        let second: Value = serde_json::from_str(
            &handle_trace_scaffold(
                &c,
                &json!({ "items": ["REQ-003"], "target_doc": "at-doc", "mode": "apply" }),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(second["applied"], false, "{second}");
        assert!(
            second["generated"].as_array().unwrap().is_empty(),
            "{second}"
        );
        assert_eq!(second["skipped"].as_array().unwrap().len(), 2, "{second}");
    }

    /// Regression: idempotency is per (from, target layer). Scaffolding the
    /// same AC into an acceptance doc must not make a later scaffold into a
    /// system_test doc skip it (registrar runs acceptance -> system_test).
    #[test]
    fn handler_apply_scaffolds_same_ac_into_two_layers_and_each_is_idempotent() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff.clone());
        let req_doc = layer_doc("doc-req", "req-doc", "requirement");
        crate::storage::docs::write_doc(&handoff, &req_doc).unwrap();
        handle_doc_save(
            &c,
            &json!({"doc_id": "doc-req", "body": "# Requirements\n\n\
                ### REQ-003 Account lockout\n\n本文。\n\n\
                受入基準:\n\
                - AC1: Given a When b Then locked\n\
                - AC2: WHEN locked THE SYSTEM SHALL reject login\n"}),
        )
        .unwrap();
        for (id, slug, layer) in [
            ("doc-at", "at-doc", "acceptance"),
            ("doc-st", "st-doc", "system_test"),
        ] {
            crate::storage::docs::write_doc(&handoff, &layer_doc(id, slug, layer)).unwrap();
            handle_doc_save(&c, &json!({"doc_id": id, "body": "# Tests\n"})).unwrap();
        }

        let run = |target: &str| -> Value {
            serde_json::from_str(
                &handle_trace_scaffold(
                    &c,
                    &json!({"doc": "req-doc", "target_doc": target, "mode": "apply"}),
                )
                .unwrap(),
            )
            .unwrap()
        };

        let at = run("at-doc");
        assert_eq!(at["generated"].as_array().unwrap().len(), 2, "{at}");

        let st = run("st-doc");
        let st_ids: Vec<&str> = st["generated"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["id"].as_str().unwrap())
            .collect();
        assert_eq!(st_ids, vec!["ST-REQ-003-1", "ST-REQ-003-2"], "{st}");
        assert!(st["skipped"].as_array().unwrap().is_empty(), "{st}");

        for target in ["at-doc", "st-doc"] {
            let again = run(target);
            assert!(again["generated"].as_array().unwrap().is_empty(), "{again}");
            assert_eq!(again["skipped"].as_array().unwrap().len(), 2, "{again}");
        }
    }

    /// NFR-006 (wiki/270 §2.7): when the resolved profile's
    /// `max_generated_per_call` is exceeded by an `apply`-mode call's actual
    /// generated count, a warning is added. Generation itself is still
    /// governed by `limit` (the safety-net only warns, never truncates).
    #[test]
    fn handler_apply_warns_when_generated_count_exceeds_profile_max() {
        let (_tmp, handoff) = setup();
        std::fs::write(
            handoff.join("config.toml"),
            "[project]\nname = \"t\"\n\n[trace.profiles.test]\nextends = \"standard\"\nmax_generated_per_call = 2\n",
        )
        .unwrap();

        let req_body = "# Requirements\n\n\
            ### REQ-020 Something\n\n\
            Statement.\n\n\
            受入基準:\n\
            - AC1: The system does thing one\n\
            - AC2: The system does thing two\n\
            - AC3: The system does thing three\n";
        let c = ctx(handoff.clone());
        let req_doc = layer_doc("doc-req3", "req-doc3", "requirement");
        crate::storage::docs::write_doc(&handoff, &req_doc).unwrap();
        handle_doc_save(&c, &json!({ "doc_id": "doc-req3", "body": req_body })).unwrap();

        let mut at_doc = layer_doc("doc-at3", "at-doc3", "acceptance");
        at_doc.trace_profile = Some("test".to_string());
        crate::storage::docs::write_doc(&handoff, &at_doc).unwrap();
        handle_doc_save(
            &c,
            &json!({ "doc_id": "doc-at3", "body": "# Acceptance\n" }),
        )
        .unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_scaffold(
                &c,
                &json!({ "items": ["REQ-020"], "target_doc": "at-doc3", "mode": "apply" }),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["generated"].as_array().unwrap().len(), 3, "{out}");
        let warnings = out["warnings"].as_array().unwrap();
        assert!(
            warnings.iter().any(|w| w
                .as_str()
                .unwrap_or("")
                .contains("Generated 3 items, exceeding profile limit of 2")),
            "expected a profile-limit warning, got: {warnings:?}"
        );
    }

    /// The same profile cap, but generation stays at/under the limit (via
    /// `limit=1`) — no warning.
    #[test]
    fn handler_apply_does_not_warn_when_within_profile_max() {
        let (_tmp, handoff) = setup();
        std::fs::write(
            handoff.join("config.toml"),
            "[project]\nname = \"t\"\n\n[trace.profiles.test]\nextends = \"standard\"\nmax_generated_per_call = 2\n",
        )
        .unwrap();

        let req_body = "# Requirements\n\n\
            ### REQ-021 Something\n\n\
            Statement.\n\n\
            受入基準:\n\
            - AC1: The system does thing one\n\
            - AC2: The system does thing two\n\
            - AC3: The system does thing three\n";
        let c = ctx(handoff.clone());
        let req_doc = layer_doc("doc-req4", "req-doc4", "requirement");
        crate::storage::docs::write_doc(&handoff, &req_doc).unwrap();
        handle_doc_save(&c, &json!({ "doc_id": "doc-req4", "body": req_body })).unwrap();

        let mut at_doc = layer_doc("doc-at4", "at-doc4", "acceptance");
        at_doc.trace_profile = Some("test".to_string());
        crate::storage::docs::write_doc(&handoff, &at_doc).unwrap();
        handle_doc_save(
            &c,
            &json!({ "doc_id": "doc-at4", "body": "# Acceptance\n" }),
        )
        .unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_scaffold(
                &c,
                &json!({ "items": ["REQ-021"], "target_doc": "at-doc4", "mode": "apply", "limit": 1 }),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["generated"].as_array().unwrap().len(), 1, "{out}");
        let warnings = out["warnings"].as_array().unwrap();
        assert!(
            !warnings
                .iter()
                .any(|w| w.as_str().unwrap_or("").contains("exceeding profile limit")),
            "did not expect a profile-limit warning, got: {warnings:?}"
        );
    }

    #[test]
    fn handler_preview_mode_does_not_write() {
        let (_tmp, handoff) = setup();
        let req_body = "# Requirements\n\n\
            ### REQ-010 Something\n\n\
            Statement.\n\n\
            受入基準:\n\
            - AC1: The system does the thing\n";
        let c = ctx(handoff.clone());
        let req_doc = layer_doc("doc-req2", "req-doc2", "requirement");
        crate::storage::docs::write_doc(&handoff, &req_doc).unwrap();
        handle_doc_save(&c, &json!({ "doc_id": "doc-req2", "body": req_body })).unwrap();

        let at_doc = layer_doc("doc-at2", "at-doc2", "acceptance");
        crate::storage::docs::write_doc(&handoff, &at_doc).unwrap();
        handle_doc_save(
            &c,
            &json!({ "doc_id": "doc-at2", "body": "# Acceptance\n" }),
        )
        .unwrap();

        let out: Value = serde_json::from_str(
            &handle_trace_scaffold(
                &c,
                &json!({ "items": ["REQ-010"], "target_doc": "at-doc2" }),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["mode"], "preview");
        assert_eq!(out["applied"], false, "{out}");
        assert_eq!(out["generated"].as_array().unwrap().len(), 1, "{out}");

        let body = crate::storage::docs::read_doc_body(&handoff, "at-doc2")
            .unwrap()
            .unwrap();
        assert_eq!(body, "# Acceptance\n", "preview must not write anything");
    }
}
