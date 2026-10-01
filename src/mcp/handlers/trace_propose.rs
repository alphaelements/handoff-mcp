//! `handoff_trace_propose` (wiki/260-vmodel-m2-design.md §4.10, M2-17,
//! FR-1004): read-only suggestion of (1) existing V-model items that may
//! already cover a task (title/notes similarity, so a caller doesn't create
//! a duplicate requirement) and (2) a ready-to-review Markdown template for
//! a *new* item, sized to the project's applicable profile.
//!
//! Input: `task_id` (loads the task's own `title`/`notes`/`scope_paths`) XOR
//! `title` (+ optional `notes`, for a task that doesn't exist yet) — exactly
//! one of `task_id`/`title` is required. `limit` (default 5) caps
//! `candidates`.
//!
//! **Candidates** (§4.10): every persisted `SubItem` with a `stable_id` (one
//! per `stable_id` — de-duplicated when the same id exists in more than one
//! document, first occurrence in `read_all_docs`'s slug-sorted order wins —
//! and never an implicit acceptance-verification item, `implicit_of: Some`,
//! §2.5: its `description` is literally its parent's AC bullet text, so the
//! parent item already represents the same match; t360.20.30), scored by
//! [`hybrid_score`] between the query text (`title`/title+notes) and the
//! item's own `SubItem.description` (its title) — the same lexical+semantic
//! blend `handoff_memory_save`'s near-duplicate detection uses
//! ([`lexsim::hybrid_jaccard`], `src/mcp/handlers/memory.rs`), applied here
//! to item titles instead of memory bodies, but computed locally rather than
//! via that function directly so the query text's own embedding is computed
//! once up front and reused, not recomputed for every candidate (see
//! [`hybrid_score`]'s own doc comment — this was a real ≥150ms-at-JA-scale
//! regression, not a preemptive optimization). Only titles are compared (§6
//! PR-5's own rationale: "類似度は title だけ（本文を読まない）") — no document
//! body is read for this scan.
//!
//! **Proposal template** (§4.10): the applicable layer set/`implicit_acceptance`
//! is resolved in the same 3-tier priority `crate::trace::engine::resolve_in_use_layers`
//! uses (`[trace] layers` explicit config ＞ the project default `[trace]
//! profile` ＞ auto), except the third tier here falls back to the built-in
//! `"standard"` profile's *shape* rather than attempting M1's per-item
//! auto-detection (a template needs one concrete profile to render against,
//! not a computed "which layers currently have items" set) — this fallback,
//! and the `[trace] layers`-without-a-name case (labeled `"custom"` in the
//! response, `implicit_acceptance` assumed `false` since raw `layers` config
//! carries no such flag), are both reported via `warnings`, not silently
//! assumed.
//!
//! The new item's layer is the **deepest left-side (definition) layer** in
//! that profile's layer set (`minimal`: `requirement` — its only left layer;
//! `standard`: `basic_spec`, deeper than `requirement`; `full`:
//! `detailed_spec` — generalizes both of §4.10's own worked examples). Its
//! `implicit_acceptance` is decided by the **target document's own**
//! `trace_profile` override when the placement step (below) finds one, else
//! the project-tier value above — same "文書の上書き ＞ プロジェクト既定"
//! priority a real layer sync gives via `resolve_doc_implicit_acceptance`
//! (`src/storage/docs/layer_sync.rs`); only this boolean is affected, never
//! which layer/profile drives the rest of the template (t360.20.30). When
//! `implicit_acceptance` is `true` (minimal-shaped), the template is that one
//! left item plus an inline 受入基準 (acceptance criteria) block — no
//! separate verification item is needed, since layer sync auto-materializes
//! one per AC (§2.5). Otherwise (`implicit_acceptance: false`,
//! standard-shaped) the template is that left item **plus** its paired
//! right-side (verification) item, the right one carrying an explicit
//! `- layer: <id>` override attribute so both can be proposed for the same
//! target document even though the document's own default `layer` is the
//! left one — rendered via [`crate::storage::docs::layer_render`] (shared
//! with `trace_scaffold`/`trace_update`) and round-trip-verified against
//! [`crate::storage::docs::layer_parse`] in this module's own tests (§4.7's
//! "描画 → 解析の往復で同じ項目になる" requirement, reused here per this
//! task's instructions).
//!
//! When the left item's layer is not the **top-most** left layer in the
//! profile's layer set (`standard`'s `basic_spec`, `full`'s `detailed_spec`
//! — both conceptually refine a shallower left layer), the left item also
//! gets a `- refines:` suggestion: the highest-ranked entry in the
//! already-computed `candidates` list that is actually a valid refines target
//! — its own layer left-side, in the applicable profile's layer set, and at a
//! strictly shallower level than the new item's (the same condition
//! [`crate::trace::engine`]'s `refines_edge_valid` enforces for a real link;
//! [`pick_refines_candidate`]) — when one exists, else an empty
//! `- refines: ` line plus a `warnings` entry asking the caller to fill it in
//! manually (t360.20.30, refined by review-rework round 2 to filter by layer
//! rather than taking the plain top of `candidates`, which could be a
//! right-side or same/deeper-level item the engine would then reject as
//! `invalid_link`; `minimal`'s only left layer is trivially its own top, so
//! it never gets this attribute).
//!
//! IDs are `<layer's default prefix>-<max existing number for that prefix
//! project-wide, +1>`, zero-padded to 3 digits (`REQ-004`) — a plain scan of
//! every already-persisted `SubItem.stable_id` (no body re-read).
//!
//! Placement (`proposal.doc`): the layer document (among documents whose own
//! `layer` equals the new item's target layer) whose `scope_paths` overlaps
//! the task's `scope_paths` — same overlap classification `handoff_claim_task`'s
//! scope-conflict advisory uses (exact match or directory-prefix match,
//! `src/storage/tasks.rs::detect_scope_conflicts`; duplicated locally here,
//! the same way `trace_scaffold::resolve_doc_by_slug_or_id` duplicates a
//! small private helper rather than widening another module's public
//! surface for one caller). When no such document exists (including the
//! `title`-only, task-less input — no task `scope_paths` to compare against
//! at all), `doc` is instead a *suggested* new slug (not created — this tool
//! never writes) derived from the layer id and the query title, flagged in
//! `warnings`.
//!
//! Creation itself is deliberately out of scope (§4.10: "作成は利用者の確認後に
//! …で行う") — this tool only ever reads. The description text below names
//! `doc_save`/`doc_update_section` as today's stand-in for actually writing
//! the proposed Markdown, since `handoff_trace_update`'s `upsert_item` op
//! (the eventual, purpose-built replacement, M2-14) does not exist yet.
//!
//! Read-only (E6, `router::READ_ONLY_TOOLS`): only `read_all_docs`,
//! `read_config`, and (when `task_id` is given) `find_task_dir_by_id`/
//! `read_task` are called — no `runs::sync`, no layer re-sync, no derived
//! file of any kind. Verified end to end by `tests/trace_propose_e2e.rs`'s
//! byte-invariance snapshot.

use std::collections::HashSet;

use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::{json, Value};

use super::HandlerContext;
use crate::semantic::semantic_model;
use crate::storage::config::read_config;
use crate::storage::docs::layer::{LayerRegistry, LayerSide, RegisteredLayer};
use crate::storage::docs::layer_render::{render_item, ItemRenderAttrs};
use crate::storage::docs::read_all_docs;
use crate::storage::tasks::{find_task_dir_by_id, read_task, suggest_task_id};
use crate::trace::profile::{resolve_profile_by_name, resolve_project_profile};

const DEFAULT_LIMIT: u64 = 5;
/// Same weight `handoff_memory_save`'s near-duplicate detection uses
/// (`memory.rs::HYBRID_DUP_ALPHA`) — an even lexical/semantic blend, not
/// re-tuned here (no evidence this tool's title-similarity use case needs a
/// different balance than memory's near-dup detection does).
const HYBRID_ALPHA: f32 = 0.5;
const DEFAULT_HEADING_LEVEL: u8 = 3;
/// Zero-pad width for a freshly allocated id's numeric suffix
/// (`REQ-004`) — matches every worked example in wiki/260 §2.2/§4.7
/// (`REQ-003`, `SPEC-020`, `ST-051`, `AT-REQ-003-1`).
const ID_NUMBER_WIDTH: usize = 3;
/// Floor for how many lexically-prefiltered candidates get a full (semantic)
/// [`hybrid_score`] — see the candidate-scan doc comment at the call site
/// (§6 perf) for why this bound exists at all. `limit.saturating_mul(20)`
/// wins over this floor for an unusually large caller-requested `limit`.
const SEMANTIC_POOL_SIZE: usize = 100;

#[derive(Debug, Clone, Serialize)]
struct Candidate {
    id: String,
    title: String,
    layer: Option<String>,
    score: f64,
}

fn to_json(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Same blend [`lexsim::hybrid_jaccard`] computes, but takes a
/// **precomputed** query embedding instead of re-embedding the query text on
/// every call — see the call site's doc comment (§6 perf) for why. Falls
/// back to a neutral `0.5` semantic score exactly like `hybrid_jaccard` does
/// when an embedding is unavailable (either `query_embedding` is `None` — the
/// query itself failed to embed — or `candidate` fails to embed).
fn hybrid_score(
    query_embedding: Option<&[f32]>,
    query_text: &str,
    candidate: &str,
    model: &lexsim::semantic::SemanticModelView<'_>,
    alpha: f32,
) -> f64 {
    let alpha = if alpha.is_finite() {
        alpha.clamp(0.0, 1.0)
    } else {
        0.7
    } as f64;
    let lexical = lexsim::jaccard(query_text, candidate);
    let semantic = match (query_embedding, model.embed(candidate).ok()) {
        (Some(q), Some(c)) => {
            let dot: f32 = q.iter().zip(c.iter()).map(|(a, b)| a * b).sum();
            ((dot as f64 + 1.0) / 2.0).clamp(0.0, 1.0)
        }
        _ => 0.5,
    };
    alpha * lexical + (1.0 - alpha) * semantic
}

pub fn handle_trace_propose(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let task_id_arg = arguments.get("task_id").and_then(|v| v.as_str());
    let title_arg = arguments.get("title").and_then(|v| v.as_str());
    let notes_arg = arguments.get("notes").and_then(|v| v.as_str());
    if task_id_arg.is_some() && title_arg.is_some() {
        bail!("'task_id' and 'title' are mutually exclusive");
    }
    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_LIMIT) as usize;

    let (title, notes, scope_paths): (String, String, Vec<String>) =
        if let Some(task_id) = task_id_arg {
            let tasks_dir = handoff.join("tasks");
            let task_dir = find_task_dir_by_id(&tasks_dir, task_id)?
                .ok_or_else(|| anyhow::anyhow!("{}", suggest_task_id(&tasks_dir, task_id)))?;
            let (data, _status) = read_task(&task_dir)?
                .ok_or_else(|| anyhow::anyhow!("Task file not found in {}", task_dir.display()))?;
            (data.title, data.notes.unwrap_or_default(), data.scope_paths)
        } else if let Some(title) = title_arg {
            (
                title.to_string(),
                notes_arg.unwrap_or_default().to_string(),
                Vec::new(),
            )
        } else {
            bail!("either 'task_id' or 'title' is required");
        };

    let query_text = if notes.is_empty() {
        title.clone()
    } else {
        format!("{title} {notes}")
    };

    let all_docs = read_all_docs(handoff)?;
    let trace_config = read_config(&handoff.join("config.toml"))
        .map(|c| c.trace)
        .unwrap_or_default();
    let registry = LayerRegistry::build(&trace_config.layer);
    let mut warnings: Vec<String> = registry.warnings.clone();

    // -- candidates (§4.10: existing-item similarity, title only) --
    //
    // Perf (§6, PR-5 "≤ 150 ms"), two fixes together:
    //
    // 1. Calling `lexsim::hybrid_jaccard(query, ..)` once per candidate
    //    re-embeds `query` itself on every single call
    //    (`SemanticModelView::similarity` always embeds both of its
    //    arguments fresh, with no cache). `query_embedding` is computed
    //    exactly once up front and reused for every candidate instead, via
    //    [`hybrid_score`] — the same blend `lexsim::hybrid_jaccard` computes,
    //    just without the redundant re-embed.
    //
    // 2. Even with (1), embedding *every* candidate at NFR-003's JA/L scale
    //    (the fixed 2,500-item trace fixture plus that scale's own
    //    thousands of base-fixture subitems) still does not fit the budget
    //    (measured 134.9ms at S scale — the smallest one — with only ~15ms
    //    of headroom before M/L/JA's larger item counts). So the embedding
    //    step only ever runs over a bounded top-`SEMANTIC_POOL_SIZE` (or
    //    `limit`-scaled, whichever is larger) pool selected by the *lexical*
    //    Jaccard score alone first (`lexsim::jaccard`, cheap — no embedding)
    //    — the same two-stage "cheap prefilter, expensive rerank" shape
    //    `context/injection.rs`'s BM25-then-semantic hybrid search already
    //    uses for a similar reason. This means a candidate with near-zero
    //    lexical overlap but a very high semantic similarity to the query
    //    could be missed when the corpus is larger than the pool — an
    //    accepted trade-off for a `limit`-5-by-default "did we already write
    //    this down?" suggestion, not an exhaustive search (documented here,
    //    not silently different behavior at different corpus sizes).
    let model = semantic_model();
    let query_embedding = model.embed(&query_text).ok();

    struct LexicalCandidate {
        stable_id: String,
        title: String,
        layer: Option<String>,
        lexical: f64,
    }

    let mut existing_ids: HashSet<String> = HashSet::new();
    // De-duplicates `lexical_pool` by `stable_id` (t360.20.30, M2-S6
    // reviewer/developer B finding): the same stable_id can appear in more
    // than one document when IDs collide across documents, which would
    // otherwise surface as the same candidate twice. First occurrence wins,
    // in `read_all_docs`'s deterministic slug-sorted document order — stable
    // run to run, not dependent on in-memory iteration/hash order.
    let mut candidate_stable_ids: HashSet<String> = HashSet::new();
    let mut lexical_pool: Vec<LexicalCandidate> = Vec::new();
    for doc in &all_docs {
        let Some(verification) = &doc.verification else {
            continue;
        };
        for item in &verification.items {
            for sub in &item.sub_items {
                let Some(stable_id) = &sub.stable_id else {
                    continue;
                };
                existing_ids.insert(stable_id.clone());
                // Implicit acceptance-verification items (§2.5 step 3,
                // `implicit_of: Some(..)`) have a `description` that is
                // literally their parent's AC bullet text — excluded from
                // candidates entirely (not merely flagged): it both
                // duplicates the parent item's own signal and would rank
                // candidates by AC phrasing rather than the requirement's own
                // statement, and the parent item already represents the same
                // "did we already write this down?" match target.
                if sub.implicit_of.is_some() {
                    continue;
                }
                if sub.description.is_empty() {
                    continue;
                }
                if !candidate_stable_ids.insert(stable_id.clone()) {
                    continue;
                }
                let lexical = lexsim::jaccard(&query_text, &sub.description);
                lexical_pool.push(LexicalCandidate {
                    stable_id: stable_id.clone(),
                    title: sub.description.clone(),
                    layer: sub.layer.clone().or_else(|| doc.layer.clone()),
                    lexical,
                });
            }
        }
    }
    lexical_pool.sort_by(|a, b| {
        b.lexical
            .partial_cmp(&a.lexical)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.stable_id.cmp(&b.stable_id))
    });
    let pool_size = SEMANTIC_POOL_SIZE.max(limit.saturating_mul(20));
    lexical_pool.truncate(pool_size);

    let mut scored: Vec<Candidate> = lexical_pool
        .into_iter()
        .map(|c| {
            let score = hybrid_score(
                query_embedding.as_deref(),
                &query_text,
                &c.title,
                model,
                HYBRID_ALPHA,
            );
            Candidate {
                id: c.stable_id,
                title: c.title,
                layer: c.layer,
                score,
            }
        })
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    // t360.20.31: `candidates` (the `limit`-capped response field below) is
    // not the only pool a `refines:` suggestion may draw from — keep the
    // full (still pool-bounded, just not `limit`-bounded) ranked list around
    // so [`pick_refines_candidate`] has somewhere to fall back to when
    // nothing within the top-`limit` entries is a valid refines target (a
    // caller's small `limit` must not silently starve the suggestion of an
    // otherwise-available valid candidate ranked just outside it).
    let ranked_pool = scored.clone();
    scored.truncate(limit);
    for c in &mut scored {
        c.score = round2(c.score);
    }
    let candidates = scored;

    // -- proposal (§4.10: profile-shaped new-item template) --
    let (resolved_default, profile_warnings) = resolve_project_profile(&trace_config, &registry);
    warnings.extend(profile_warnings);

    let configured_profile = trace_config.profile.as_deref().filter(|s| !s.is_empty());
    let (profile_name, profile_layers, implicit_acceptance): (String, Vec<String>, bool) =
        if !trace_config.layers.is_empty() {
            // Tier 1 (§2.1: `[trace] layers` ＞ profile ＞ auto) — the layer
            // set comes from `[trace] layers`. `implicit_acceptance` is a
            // profile-only concept: when a named project profile resolves,
            // use its flag — exactly what layer sync itself does for this
            // project (`resolve_doc_implicit_acceptance`, which never looks
            // at `[trace] layers`), so the template matches how the saved
            // item will actually be synced. Only the "layers without a
            // (resolvable) named profile" case is labeled `"custom"` and
            // assumes `false` (§4.10's 実装メモ: "明示のみ（プロファイル名なし）").
            match resolved_default {
                Some(p) => (p.name, trace_config.layers.clone(), p.implicit_acceptance),
                None => {
                    warnings.push(
                        "[trace] layers is set without a resolvable named profile; \
                         trace_propose assumes implicit_acceptance=false (explicit \
                         verification items) for its template"
                            .to_string(),
                    );
                    ("custom".to_string(), trace_config.layers.clone(), false)
                }
            }
        } else if let Some(p) = resolved_default {
            (p.name, p.layers, p.implicit_acceptance)
        } else {
            let (fallback, fallback_warnings) =
                resolve_profile_by_name("standard", &trace_config, &registry);
            warnings.extend(fallback_warnings);
            warnings.push(match configured_profile {
                Some(name) => format!(
                    "[trace] profile '{name}' could not be resolved; trace_propose defaults \
                     to the 'standard' profile shape for its template"
                ),
                None => "no [trace] profile or [trace] layers is configured; trace_propose \
                         defaults to the 'standard' profile shape for its template"
                    .to_string(),
            });
            match fallback {
                Some(p) => (p.name, p.layers, p.implicit_acceptance),
                None => {
                    warnings.push(
                        "could not resolve a template layer set (even the built-in 'standard' \
                         fallback failed); no item template proposal is available"
                            .to_string(),
                    );
                    return Ok(to_json(&json!({
                        "candidates": candidates,
                        "proposal": Value::Null,
                        "warnings": warnings,
                    })));
                }
            }
        };

    let left_layer: Option<&RegisteredLayer> = profile_layers
        .iter()
        .filter_map(|l| registry.get(l))
        .filter(|l| l.side == LayerSide::Left)
        .max_by_key(|l| l.level);

    let Some(left_layer) = left_layer else {
        warnings.push(format!(
            "profile '{profile_name}' has no left-side (definition) layer in its layer set; \
             no item template proposal is available"
        ));
        return Ok(to_json(&json!({
            "candidates": candidates,
            "proposal": Value::Null,
            "warnings": warnings,
        })));
    };

    let left_prefix = registry
        .id_prefixes_for(&left_layer.id, &trace_config.id_prefixes)
        .first()
        .cloned()
        .unwrap_or_else(|| left_layer.id.to_uppercase());

    // Placement (§4.10): resolved *before* the template's markdown, not
    // after — the target document's own `trace_profile` override (below)
    // needs to be known before rendering the body, and placement itself only
    // depends on `left_layer`/`scope_paths`/`all_docs`, none of which the
    // markdown generation below affects.
    let target_doc = all_docs
        .iter()
        .filter(|d| d.layer.as_deref() == Some(left_layer.id.as_str()))
        .filter(|d| scope_overlap(&scope_paths, &d.scope_paths))
        .min_by(|a, b| a.slug.cmp(&b.slug));

    // t360.20.30 item 3 (wiki/260 §2.1/§2.5 手順3's "文書の上書き ＞ プロジェクト
    // 既定"): the *target* document's own `trace_profile` decides this
    // template's `implicit_acceptance`, the same priority
    // `resolve_doc_implicit_acceptance` (`src/storage/docs/layer_sync.rs`)
    // gives a real layer sync — not reused directly here, since that
    // function's own project-default branch only ever considers `[trace]
    // profile` (via `resolve_project_profile`), which would throw away the
    // richer `[trace] layers`/fallback-to-standard tiering already resolved
    // above into `implicit_acceptance`; only its document-override half is
    // needed here, so it is inlined. This only ever changes the
    // inline-acceptance-block-vs-paired-verification-item shape of the
    // template — never `left_layer`/`profile_layers` themselves (out of this
    // task's scope; the target document was already chosen above using the
    // project-level layer set).
    let implicit_acceptance = match target_doc
        .and_then(|d| d.trace_profile.as_deref())
        .filter(|s| !s.is_empty())
    {
        Some(name) => {
            let (resolved, doc_profile_warnings) =
                resolve_profile_by_name(name, &trace_config, &registry);
            warnings.extend(doc_profile_warnings);
            match resolved {
                Some(p) => {
                    warnings.push(format!(
                        "target document '{}' trace_profile '{name}' decides this template's \
                         implicit_acceptance ({}), overriding the project default",
                        target_doc.map(|d| d.slug.as_str()).unwrap_or_default(),
                        p.implicit_acceptance
                    ));
                    p.implicit_acceptance
                }
                None => {
                    warnings.push(format!(
                        "target document '{}' trace_profile '{name}' could not be resolved; \
                         keeping the project-level implicit_acceptance for this template",
                        target_doc.map(|d| d.slug.as_str()).unwrap_or_default()
                    ));
                    implicit_acceptance
                }
            }
        }
        None => implicit_acceptance,
    };

    // t360.20.30 item 2: a left-side layer that is not the top-most left
    // layer in the applicable profile (e.g. `standard`'s `basic_spec`,
    // `full`'s `detailed_spec`) conceptually refines something shallower —
    // the template gets a `refines:` suggestion from the highest-ranked
    // candidate that is actually a *valid* refines target (review-rework
    // round 2, t360.20.30 MAJOR: the plain top of `candidates` can be a
    // right-side or same/deeper-level item — `candidates` ranks by title
    // similarity alone, with no layer filter — which `crate::trace::engine`'s
    // `refines_edge_valid` then rejects as `invalid_link` the moment the
    // suggestion is saved, even though a valid, merely lower-ranked candidate
    // was available; see [`pick_refines_candidate`]). When no candidate
    // qualifies (including when `candidates` is empty), the line is still
    // emitted (so the author sees where to fill it in) with an empty value,
    // and the omission is reported via `warnings` rather than silently
    // leaving the attribute off the way a top-most-left-layer template does.
    //
    // t360.20.31 (M2-S7 reviewer finding): a small caller-supplied `limit`
    // must not starve this suggestion of an otherwise-available valid
    // candidate — the first pick is still restricted to the `limit`-capped
    // `candidates` (so the suggested id is, when possible, one the caller
    // can already see in the same response), but when *none* of those
    // qualify, a second, independent search over `ranked_pool` (the same
    // ranking, not `limit`-truncated) looks for the top-ranked valid
    // candidate beyond the cap before giving up.
    let topmost_left_level = profile_layers
        .iter()
        .filter_map(|l| registry.get(l))
        .filter(|l| l.side == LayerSide::Left)
        .map(|l| l.level)
        .min();
    let left_refines: Vec<String> = if topmost_left_level.is_some_and(|lvl| lvl < left_layer.level)
    {
        let picked = pick_refines_candidate(&candidates, &registry, &profile_layers, left_layer)
            .or_else(|| {
                pick_refines_candidate(&ranked_pool, &registry, &profile_layers, left_layer)
            });
        match picked {
            Some(c) => vec![c.id.clone()],
            None => {
                warnings.push(format!(
                    "no similar existing upper-layer item found to suggest as this template's \
                     'refines:' target (layer '{}' is not the top-most left layer in profile \
                     '{profile_name}'); fill in the upstream id manually",
                    left_layer.id
                ));
                vec![String::new()]
            }
        }
    } else {
        Vec::new()
    };

    let statement = if notes.is_empty() {
        "（記入）".to_string()
    } else {
        notes.clone()
    };

    let (markdown, next_ids) = if implicit_acceptance {
        let left_id = format_next_id(
            &left_prefix,
            next_number_for_prefix(&existing_ids, &left_prefix),
        );
        let left_statement = format!("{statement}\n\n受入基準:\n- AC1: （記入）");
        let left_attrs = ItemRenderAttrs {
            refines: left_refines,
            ..Default::default()
        };
        let rendered = render_item(
            DEFAULT_HEADING_LEVEL,
            &left_id,
            &title,
            &left_attrs,
            &left_statement,
        );
        (rendered, vec![left_id])
    } else {
        let Some(right_layer) = registry.get(&left_layer.pair) else {
            warnings.push(format!(
                "layer '{}' has no reciprocal pair in the registry; no item template proposal \
                 is available",
                left_layer.id
            ));
            return Ok(to_json(&json!({
                "candidates": candidates,
                "proposal": Value::Null,
                "warnings": warnings,
            })));
        };
        let right_prefix = registry
            .id_prefixes_for(&right_layer.id, &trace_config.id_prefixes)
            .first()
            .cloned()
            .unwrap_or_else(|| right_layer.id.to_uppercase());

        let left_id = format_next_id(
            &left_prefix,
            next_number_for_prefix(&existing_ids, &left_prefix),
        );
        let right_id = format_next_id(
            &right_prefix,
            next_number_for_prefix(&existing_ids, &right_prefix),
        );

        let left_attrs = ItemRenderAttrs {
            refines: left_refines,
            ..Default::default()
        };
        let left_rendered = render_item(
            DEFAULT_HEADING_LEVEL,
            &left_id,
            &title,
            &left_attrs,
            &statement,
        );
        let right_attrs = ItemRenderAttrs {
            verifies: vec![left_id.clone()],
            layer: Some(right_layer.id.clone()),
            ..Default::default()
        };
        let right_title = format!("{title} の検証");
        let right_rendered = render_item(
            DEFAULT_HEADING_LEVEL,
            &right_id,
            &right_title,
            &right_attrs,
            "（記入）",
        );
        (
            format!("{left_rendered}\n\n{right_rendered}"),
            vec![left_id, right_id],
        )
    };

    let doc_field = match target_doc {
        Some(d) => d.slug.clone(),
        None => {
            let seed = if title.is_empty() {
                &left_layer.id
            } else {
                &title
            };
            let suggested = suggest_doc_slug(&left_layer.id, seed);
            warnings.push(format!(
                "no existing '{}' layer document overlaps this task's scope_paths; suggesting \
                 a new document slug '{suggested}' (not created — trace_propose is read-only)",
                left_layer.id
            ));
            suggested
        }
    };

    Ok(to_json(&json!({
        "candidates": candidates,
        "proposal": {
            "profile": profile_name,
            "doc": doc_field,
            "markdown": markdown,
            "next_ids": next_ids,
        },
        "warnings": warnings,
    })))
}

/// The next free number for `prefix` (`"REQ"` -> looks for `"REQ-<digits>"`
/// among `existing_ids`, ignoring a trailing lowercase-letter suffix like
/// `trace_scaffold`'s collision-avoidance scheme uses — an id shaped that
/// way is a collision-avoidance variant of some other number, not this
/// scan's own sequence) — `1` when `prefix` has no existing numbered id at
/// all.
fn next_number_for_prefix(existing_ids: &HashSet<String>, prefix: &str) -> u32 {
    let needle = format!("{prefix}-");
    let mut max = 0u32;
    for id in existing_ids {
        let Some(rest) = id.strip_prefix(&needle) else {
            continue;
        };
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            continue;
        }
        let after_digits = &rest[digits.len()..];
        if !after_digits.chars().all(|c| c.is_ascii_lowercase()) {
            continue; // not a plain "<prefix>-<digits>[a-z]?" id (e.g. a compound scaffold id).
        }
        if let Ok(n) = digits.parse::<u32>() {
            max = max.max(n);
        }
    }
    max + 1
}

fn format_next_id(prefix: &str, number: u32) -> String {
    format!("{prefix}-{number:0width$}", width = ID_NUMBER_WIDTH)
}

/// The highest-ranked entry of `pool` (already sorted by [`hybrid_score`])
/// that is actually a *valid* `refines:` target for `left_layer`: its own
/// layer must be left-side, in `profile_layers` (the applicable profile's own
/// layer set — a candidate from a layer outside today's profile is not a
/// link `trace_scaffold`/layer sync would recognize as in-scope either), and
/// at a strictly shallower level than `left_layer` — exactly the condition
/// `crate::trace::engine::refines_edge_valid` enforces for a real link
/// (review-rework round 2, t360.20.30 MAJOR finding: picking the plain top of
/// `candidates` regardless of layer let the tool suggest a link its own
/// engine would then reject as `invalid_link`). `None` when no entry of
/// `pool` qualifies (including when `pool` is empty).
///
/// Called twice at the one call site (t360.20.31): first against the
/// `limit`-capped `candidates`, then — only if that found nothing — against
/// `ranked_pool`, the same ranking without the `limit` cap, so a small
/// caller-supplied `limit` can't hide an otherwise-available valid candidate
/// ranked just outside it. Either call's `None` ultimately falls back to an
/// empty `refines:` line plus a `warnings` entry.
fn pick_refines_candidate<'a>(
    pool: &'a [Candidate],
    registry: &LayerRegistry,
    profile_layers: &[String],
    left_layer: &RegisteredLayer,
) -> Option<&'a Candidate> {
    pool.iter().find(|c| {
        c.layer.as_deref().is_some_and(|layer_id| {
            profile_layers.iter().any(|l| l == layer_id)
                && registry
                    .get(layer_id)
                    .is_some_and(|l| l.side == LayerSide::Left && l.level < left_layer.level)
        })
    })
}

/// Same overlap classification as `src/storage/tasks.rs`'s
/// `classify_path_overlap` (exact match, or one path is a directory-prefix of
/// the other) — duplicated locally rather than exported from `tasks.rs` for
/// one caller (see this module's doc comment).
fn paths_overlap(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let a_dir = a.strip_suffix('/').unwrap_or(a);
    let b_dir = b.strip_suffix('/').unwrap_or(b);
    b.starts_with(&format!("{a_dir}/")) || a.starts_with(&format!("{b_dir}/"))
}

fn scope_overlap(task_paths: &[String], doc_paths: &[String]) -> bool {
    if task_paths.is_empty() || doc_paths.is_empty() {
        return false;
    }
    task_paths
        .iter()
        .any(|t| doc_paths.iter().any(|d| paths_overlap(t, d)))
}

/// A suggested (never created) document slug for a layer with no existing
/// scope-overlapping document: `"<layer>-<slugified seed>"`, ASCII-lowercase
/// alphanumeric words joined by single dashes, truncated to `DocMetadata`'s
/// 60-char slug limit. Falls back to `"<layer>-<8-hex-char FNV of seed>"`
/// when the seed slugifies to nothing at all (an all-Japanese/non-ASCII
/// title, common in this project — `slugify` only recognizes ASCII
/// alphanumerics, matching `DocMetadata.slug`'s own `[a-z0-9-]` contract),
/// so the suggestion is always a valid slug candidate regardless of the
/// title's script.
fn suggest_doc_slug(layer: &str, seed: &str) -> String {
    let slug = slugify(seed);
    if slug.is_empty() {
        let hash = lexsim::fnv1a_hex(seed.as_bytes());
        let short: String = hash.chars().take(8).collect();
        return format!("{layer}-{short}");
    }
    let mut combined = format!("{layer}-{slug}");
    combined.truncate(60);
    while combined.ends_with('-') {
        combined.pop();
    }
    combined
}

fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true; // suppress a leading dash
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::layer_parse::{default_prefix_table, parse_layer_body};
    use crate::storage::docs::{write_doc, DocMetadata, SubItem, Verification, VerificationItem};
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

    fn sub_item(stable_id: &str, description: &str, layer: Option<&str>) -> SubItem {
        SubItem {
            index: 0,
            description: description.to_string(),
            status: "pending".to_string(),
            reviewer: None,
            verified_at: None,
            notes: String::new(),
            category: "requirement".to_string(),
            stable_id: Some(stable_id.to_string()),
            priority: None,
            dev_stage: None,
            impl_refs: Vec::new(),
            test_refs: Vec::new(),
            task_ids: Vec::new(),
            depends_on: Vec::new(),
            origin: Some("body".to_string()),
            layer: layer.map(str::to_string),
            refines: Vec::new(),
            verifies: Vec::new(),
            method: None,
            ..Default::default()
        }
    }

    /// A minimal layer document carrying one verification item with the
    /// given sub_items — enough for the candidate scan, which only reads
    /// already-persisted metadata (never doc bodies).
    fn layer_doc_with_items(
        id: &str,
        slug: &str,
        layer: &str,
        scope_paths: &[&str],
        subs: Vec<SubItem>,
    ) -> DocMetadata {
        let mut doc = DocMetadata::new(
            id.to_string(),
            slug.to_string(),
            format!("Title {id}"),
            "spec".to_string(),
            "2026-09-28T00:00:00Z".to_string(),
        );
        doc.layer = Some(layer.to_string());
        doc.scope_paths = scope_paths.iter().map(|s| s.to_string()).collect();
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: "2026-09-28T00:00:00Z".to_string(),
            updated_at: "2026-09-28T00:00:00Z".to_string(),
            items: vec![VerificationItem {
                fragment_seq: None,
                label: Some("item-1".to_string()),
                heading: String::new(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "requirement".to_string(),
                sub_items: subs,
            }],
        });
        doc
    }

    #[test]
    fn requires_exactly_one_of_task_id_or_title() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let err = handle_trace_propose(&c, &json!({})).unwrap_err();
        assert!(err.to_string().contains("required"), "{err}");
    }

    #[test]
    fn rejects_both_task_id_and_title() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let err = handle_trace_propose(&c, &json!({"task_id": "t1", "title": "x"})).unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"), "{err}");
    }

    #[test]
    fn errors_on_unknown_task_id() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let err = handle_trace_propose(&c, &json!({"task_id": "nope"})).unwrap_err();
        assert!(err.to_string().contains("nope") || err.to_string().contains("not found"));
    }

    #[test]
    fn candidates_are_ranked_by_title_similarity_and_limited() {
        let (_tmp, handoff) = setup();
        let doc = layer_doc_with_items(
            "doc-1",
            "req-doc",
            "requirement",
            &[],
            vec![
                sub_item(
                    "REQ-001",
                    "Account lockout after failed logins",
                    Some("requirement"),
                ),
                sub_item(
                    "REQ-002",
                    "Unrelated billing invoice export",
                    Some("requirement"),
                ),
            ],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_trace_propose(
                &c,
                &json!({"title": "Account lockout after too many failed logins", "limit": 1}),
            )
            .unwrap(),
        )
        .unwrap();
        let candidates = out["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 1, "{out}");
        assert_eq!(candidates[0]["id"], "REQ-001", "{out}");
        assert_eq!(candidates[0]["layer"], "requirement", "{out}");
    }

    #[test]
    fn candidate_layer_falls_back_to_doc_layer_when_sub_item_has_no_override() {
        let (_tmp, handoff) = setup();
        let doc = layer_doc_with_items(
            "doc-1",
            "req-doc",
            "requirement",
            &[],
            vec![sub_item("REQ-001", "Some requirement", None)],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_trace_propose(&c, &json!({"title": "Some requirement"})).unwrap(),
        )
        .unwrap();
        assert_eq!(out["candidates"][0]["layer"], "requirement", "{out}");
    }

    #[test]
    fn minimal_profile_proposes_a_single_requirement_item_with_acceptance_block() {
        let (_tmp, handoff) = setup();
        let config = "[project]\nname = \"t\"\n\n[trace]\nprofile = \"minimal\"\n";
        std::fs::write(handoff.join("config.toml"), config).unwrap();

        let c = ctx(handoff.clone());
        let out: Value = serde_json::from_str(
            &handle_trace_propose(
                &c,
                &json!({"title": "Account lockout", "notes": "5 fails locks 15 min"}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["proposal"]["profile"], "minimal", "{out}");
        let next_ids = out["proposal"]["next_ids"].as_array().unwrap();
        assert_eq!(next_ids.len(), 1, "{out}");
        assert_eq!(next_ids[0], "REQ-001", "{out}");
        let markdown = out["proposal"]["markdown"].as_str().unwrap();
        assert!(markdown.contains("REQ-001"), "{markdown}");
        assert!(markdown.contains("受入基準"), "{markdown}");

        // Round-trip (§4.7's requirement, reused here): rendering then
        // parsing back reproduces the same item.
        let registry = LayerRegistry::build(&[]);
        let prefix_table = default_prefix_table(&registry, &Default::default());
        let body = format!("# Requirements\n\n{markdown}\n");
        let parsed = parse_layer_body(&body, Some("requirement"), &prefix_table);
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].id, "REQ-001");
        assert_eq!(parsed.items[0].acceptance.len(), 1);
    }

    #[test]
    fn standard_profile_proposes_a_paired_spec_and_system_test_item() {
        let (_tmp, handoff) = setup();
        let config = "[project]\nname = \"t\"\n\n[trace]\nprofile = \"standard\"\n";
        std::fs::write(handoff.join("config.toml"), config).unwrap();

        let c = ctx(handoff.clone());
        let out: Value = serde_json::from_str(
            &handle_trace_propose(&c, &json!({"title": "Audit log retention"})).unwrap(),
        )
        .unwrap();
        assert_eq!(out["proposal"]["profile"], "standard", "{out}");
        let next_ids: Vec<&str> = out["proposal"]["next_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(next_ids, vec!["SPEC-001", "ST-001"], "{out}");

        let markdown = out["proposal"]["markdown"].as_str().unwrap();
        let registry = LayerRegistry::build(&[]);
        let prefix_table = default_prefix_table(&registry, &Default::default());
        let body = format!("# Spec\n\n{markdown}\n");
        let parsed = parse_layer_body(&body, Some("basic_spec"), &prefix_table);
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        assert_eq!(parsed.items.len(), 2, "{:?}", parsed.items);
        assert_eq!(parsed.items[0].id, "SPEC-001");
        assert_eq!(parsed.items[0].attrs.layer, None);
        assert_eq!(parsed.items[1].id, "ST-001");
        // The right item explicitly overrides its layer to `system_test`
        // even though the surrounding document's own default is
        // `basic_spec` — this is what lets both items live in one proposed
        // Markdown block despite belonging to different layers.
        assert_eq!(parsed.items[1].attrs.layer.as_deref(), Some("system_test"));
        assert_eq!(parsed.items[1].attrs.verifies, vec!["SPEC-001".to_string()]);
    }

    #[test]
    fn next_id_increments_past_the_highest_existing_number_for_that_prefix() {
        let (_tmp, handoff) = setup();
        // minimal: requirement is its only left layer, so the new item's
        // prefix is deterministically REQ- regardless of the no-config
        // 'standard' fallback's own deepest-left choice (basic_spec/SPEC-).
        std::fs::write(
            handoff.join("config.toml"),
            "[project]\nname = \"t\"\n\n[trace]\nprofile = \"minimal\"\n",
        )
        .unwrap();
        let doc = layer_doc_with_items(
            "doc-1",
            "req-doc",
            "requirement",
            &[],
            vec![
                sub_item("REQ-003", "Existing one", Some("requirement")),
                sub_item("REQ-010", "Existing two", Some("requirement")),
                // A collision-avoidance-suffixed id must not be read as a
                // plain numbered id (would wrongly bump the max to 10 twice).
                sub_item("REQ-010a", "Existing two variant", Some("requirement")),
            ],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff);
        let out: Value =
            serde_json::from_str(&handle_trace_propose(&c, &json!({"title": "New one"})).unwrap())
                .unwrap();
        assert_eq!(out["proposal"]["next_ids"][0], "REQ-011", "{out}");
    }

    #[test]
    fn places_proposal_in_the_layer_document_whose_scope_overlaps_the_task() {
        let (_tmp, handoff) = setup();
        // minimal: requirement is its only left layer, matching the
        // "requirement"-layer document below (the no-config 'standard'
        // fallback would instead target a basic_spec document here).
        std::fs::write(
            handoff.join("config.toml"),
            "[project]\nname = \"t\"\n\n[trace]\nprofile = \"minimal\"\n",
        )
        .unwrap();
        let doc = layer_doc_with_items(
            "doc-1",
            "auth-requirements",
            "requirement",
            &["src/auth/"],
            vec![],
        );
        write_doc(&handoff, &doc).unwrap();

        let tasks_dir = handoff.join("tasks");
        let task_dir = tasks_dir.join("t1-auth");
        std::fs::create_dir_all(&task_dir).unwrap();
        let data = crate::storage::tasks::TaskData {
            id: "t1".to_string(),
            title: "Add account lockout".to_string(),
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
            scope_paths: vec!["src/auth/login.rs".to_string()],
            extra: Default::default(),
        };
        crate::storage::tasks::write_task(&task_dir, "todo", &data).unwrap();

        let c = ctx(handoff);
        let out: Value =
            serde_json::from_str(&handle_trace_propose(&c, &json!({"task_id": "t1"})).unwrap())
                .unwrap();
        assert_eq!(out["proposal"]["doc"], "auth-requirements", "{out}");
    }

    #[test]
    fn suggests_a_new_doc_slug_when_no_document_overlaps_scope() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_trace_propose(&c, &json!({"title": "Account Lockout Rules"})).unwrap(),
        )
        .unwrap();
        let doc = out["proposal"]["doc"].as_str().unwrap();
        assert!(doc.starts_with("basic_spec-account-lockout-rules"), "{doc}");
        assert!(
            out["warnings"].as_array().unwrap().iter().any(|w| w
                .as_str()
                .unwrap()
                .contains("suggesting a new document slug")),
            "{out}"
        );
    }

    #[test]
    fn suggest_doc_slug_falls_back_to_a_hash_for_non_ascii_titles() {
        let slug = suggest_doc_slug("requirement", "受入基準のテスト");
        assert!(slug.starts_with("requirement-"), "{slug}");
        assert_eq!(slug.len(), "requirement-".len() + 8, "{slug}");
    }

    #[test]
    fn custom_layers_without_a_named_profile_are_labeled_custom_and_warn() {
        let (_tmp, handoff) = setup();
        let config =
            "[project]\nname = \"t\"\n\n[trace]\nlayers = [\"requirement\", \"acceptance\"]\n";
        std::fs::write(handoff.join("config.toml"), config).unwrap();

        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_trace_propose(&c, &json!({"title": "Something"})).unwrap(),
        )
        .unwrap();
        assert_eq!(out["proposal"]["profile"], "custom", "{out}");
        assert!(
            out["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w.as_str().unwrap().contains("implicit_acceptance=false")),
            "{out}"
        );
    }

    /// A project default profile that fails to resolve (a cyclic `extends`
    /// chain, §2.1's own "設定エラーで全体を止めない" rule) must not abort
    /// the whole call — it falls back to the built-in `"standard"` shape
    /// (same fallback tier as "nothing configured at all"), with the
    /// resolution failure surfaced in `warnings`, not swallowed.
    #[test]
    fn unresolvable_extends_profile_falls_back_to_standard_and_reports_the_cycle() {
        let (_tmp, handoff) = setup();
        let config = "[project]\nname = \"t\"\n\n[trace]\nprofile = \"cyclic\"\n\n\
                       [trace.profiles.cyclic]\nextends = \"cyclic\"\n";
        std::fs::write(handoff.join("config.toml"), config).unwrap();

        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_trace_propose(&c, &json!({"title": "Something"})).unwrap(),
        )
        .unwrap();
        assert_eq!(out["proposal"]["profile"], "standard", "{out}");
        assert!(out["candidates"].as_array().unwrap().is_empty(), "{out}");
        let warnings: Vec<&str> = out["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap())
            .collect();
        assert!(warnings.iter().any(|w| w.contains("cycle")), "{out}");
        // A profile *is* configured here — the fallback warning must say it
        // failed to resolve, not claim nothing is configured.
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("'cyclic' could not be resolved")),
            "{out}"
        );
        assert!(
            !warnings.iter().any(|w| w.contains("no [trace] profile")),
            "{out}"
        );
    }

    /// `[trace] layers` plus a named `[trace] profile`: the layer set comes
    /// from `layers` (§2.1), but `implicit_acceptance` must still be the
    /// named profile's — layer sync resolves it that way for this project
    /// (`resolve_doc_implicit_acceptance` ignores `[trace] layers`), so a
    /// `minimal` project must get the inline-受入基準 single-item template,
    /// not a "custom"/explicit-verification-item one with a warning that
    /// wrongly claims no profile is named.
    #[test]
    fn custom_layers_with_a_named_profile_keep_that_profiles_implicit_acceptance() {
        let (_tmp, handoff) = setup();
        let config = "[project]\nname = \"t\"\n\n[trace]\nprofile = \"minimal\"\n\
                      layers = [\"requirement\", \"acceptance\"]\n";
        std::fs::write(handoff.join("config.toml"), config).unwrap();

        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_trace_propose(&c, &json!({"title": "Something"})).unwrap(),
        )
        .unwrap();
        assert_eq!(out["proposal"]["profile"], "minimal", "{out}");
        assert_eq!(
            out["proposal"]["next_ids"].as_array().unwrap().len(),
            1,
            "{out}"
        );
        assert!(
            out["proposal"]["markdown"]
                .as_str()
                .unwrap()
                .contains("受入基準"),
            "{out}"
        );
        assert!(
            !out["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w.as_str().unwrap().contains("implicit_acceptance=false")),
            "{out}"
        );
    }

    /// A custom profile whose layer set has no left-side (definition) layer
    /// at all has nothing for this tool to template — `proposal` is `null`,
    /// not a guess, and the reason is in `warnings`. `candidates` still
    /// works independently of the proposal branch.
    #[test]
    fn profile_with_no_left_side_layer_returns_a_null_proposal() {
        let (_tmp, handoff) = setup();
        let config = "[project]\nname = \"t\"\n\n[trace]\nprofile = \"right_only\"\n\n\
                       [trace.profiles.right_only]\nlayers = [\"acceptance\", \"system_test\"]\n";
        std::fs::write(handoff.join("config.toml"), config).unwrap();

        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_trace_propose(&c, &json!({"title": "Something"})).unwrap(),
        )
        .unwrap();
        assert!(out["proposal"].is_null(), "{out}");
        assert!(
            out["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w.as_str().unwrap().contains("no left-side")),
            "{out}"
        );
    }

    /// M2-S6 reviewer/developer B finding (t360.20.30, item 1): an implicit
    /// acceptance-verification `SubItem` (§2.5 step 3 — `implicit_of: Some`,
    /// `description` is literally the parent's AC bullet text) must never
    /// show up as its own candidate — it both duplicates the parent's own
    /// signal and would rank by AC phrasing rather than the requirement's own
    /// statement. Excluded, not merely flagged: it is not a distinct
    /// "did we already write this down?" match target in its own right (the
    /// parent item already represents it), and this tool's own description
    /// (`candidates` doc comment) already promises "every persisted SubItem
    /// with a stable_id" scored on title similarity, not a filtered subset
    /// callers must post-filter themselves.
    #[test]
    fn implicit_acceptance_items_are_excluded_from_candidates() {
        let (_tmp, handoff) = setup();
        let ac_text = "Given 4 prior failures When a 5th fails Then the account is locked";
        let mut parent = sub_item("REQ-003", "Account lockout policy", Some("requirement"));
        parent.acceptance = vec![];
        let mut implicit = sub_item("REQ-003#AC1", ac_text, Some("acceptance"));
        implicit.implicit_of = Some("REQ-003".to_string());
        let doc = layer_doc_with_items(
            "doc-1",
            "req-doc",
            "requirement",
            &[],
            vec![parent, implicit],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff);
        let out: Value =
            serde_json::from_str(&handle_trace_propose(&c, &json!({"title": ac_text})).unwrap())
                .unwrap();
        let candidates = out["candidates"].as_array().unwrap();
        assert!(
            candidates.iter().all(|c| c["id"] != "REQ-003#AC1"),
            "implicit item must never appear as a candidate: {out}"
        );
        assert_eq!(candidates.len(), 1, "{out}");
        assert_eq!(candidates[0]["id"], "REQ-003", "{out}");
    }

    /// M2-S6 finding, item 1 (second half): the same `stable_id` can appear
    /// in more than one document when IDs collide across documents — the
    /// candidate list must de-duplicate by `stable_id` rather than listing
    /// the same id twice. First occurrence wins, in `read_all_docs`'s
    /// deterministic slug-sorted document order (not dependent on in-memory
    /// iteration order), so the result is stable run to run.
    #[test]
    fn duplicate_stable_ids_across_documents_are_deduplicated() {
        let (_tmp, handoff) = setup();
        let doc_a = layer_doc_with_items(
            "doc-a",
            "aaa-doc",
            "requirement",
            &[],
            vec![sub_item(
                "REQ-005",
                "Account lockout after failed logins",
                Some("requirement"),
            )],
        );
        let doc_b = layer_doc_with_items(
            "doc-b",
            "zzz-doc",
            "requirement",
            &[],
            vec![sub_item(
                "REQ-005",
                "Unrelated duplicate id from another document",
                Some("requirement"),
            )],
        );
        write_doc(&handoff, &doc_a).unwrap();
        write_doc(&handoff, &doc_b).unwrap();

        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_trace_propose(
                &c,
                &json!({"title": "Account lockout after too many failed logins"}),
            )
            .unwrap(),
        )
        .unwrap();
        let candidates = out["candidates"].as_array().unwrap();
        let matching: Vec<&Value> = candidates.iter().filter(|c| c["id"] == "REQ-005").collect();
        assert_eq!(matching.len(), 1, "{out}");
        // First occurrence (slug-sorted: "aaa-doc" before "zzz-doc") wins.
        assert_eq!(
            matching[0]["title"], "Account lockout after failed logins",
            "{out}"
        );
    }

    /// M2-S6 finding, item 2: a template for a left-side layer that is *not*
    /// the top-most left layer in the applicable profile (e.g. `standard`'s
    /// `basic_spec`, which refines `requirement`) must carry a `refines:`
    /// suggestion — the top of the already-computed `candidates` list when
    /// non-empty.
    #[test]
    fn standard_profile_suggests_refines_from_the_top_candidate() {
        let (_tmp, handoff) = setup();
        let config = "[project]\nname = \"t\"\n\n[trace]\nprofile = \"standard\"\n";
        std::fs::write(handoff.join("config.toml"), config).unwrap();
        let doc = layer_doc_with_items(
            "doc-1",
            "req-doc",
            "requirement",
            &[],
            vec![sub_item(
                "REQ-001",
                "Audit log retention policy",
                Some("requirement"),
            )],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff.clone());
        let out: Value = serde_json::from_str(
            &handle_trace_propose(&c, &json!({"title": "Audit log retention"})).unwrap(),
        )
        .unwrap();
        assert_eq!(out["candidates"][0]["id"], "REQ-001", "{out}");
        let markdown = out["proposal"]["markdown"].as_str().unwrap();
        assert!(markdown.contains("- refines: REQ-001"), "{markdown}");

        let registry = LayerRegistry::build(&[]);
        let prefix_table = default_prefix_table(&registry, &Default::default());
        let body = format!("# Spec\n\n{markdown}\n");
        let parsed = parse_layer_body(&body, Some("basic_spec"), &prefix_table);
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        assert_eq!(parsed.items[0].attrs.refines, vec!["REQ-001".to_string()]);
    }

    /// M2-S6 finding, item 2 (no-candidate case): "候補の上位から、なければ空の
    /// 行と案内" — when no candidate exists at all, the template still gets a
    /// visible (empty-valued) `refines:` line, plus a `warnings` entry
    /// guiding the caller to fill it in manually, rather than silently
    /// omitting the attribute the way a top-most-left-layer template does.
    #[test]
    fn standard_profile_leaves_an_empty_refines_line_with_guidance_when_no_candidate_exists() {
        let (_tmp, handoff) = setup();
        let config = "[project]\nname = \"t\"\n\n[trace]\nprofile = \"standard\"\n";
        std::fs::write(handoff.join("config.toml"), config).unwrap();

        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_trace_propose(&c, &json!({"title": "Audit log retention"})).unwrap(),
        )
        .unwrap();
        let markdown = out["proposal"]["markdown"].as_str().unwrap();
        assert!(
            markdown.contains("- refines: \n") || markdown.contains("- refines:\n"),
            "{markdown}"
        );
        assert!(
            out["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w.as_str().unwrap().contains("refines")),
            "{out}"
        );

        let registry = LayerRegistry::build(&[]);
        let prefix_table = default_prefix_table(&registry, &Default::default());
        let body = format!("# Spec\n\n{markdown}\n");
        let parsed = parse_layer_body(&body, Some("basic_spec"), &prefix_table);
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        assert!(
            parsed.items[0].attrs.refines.is_empty(),
            "{:?}",
            parsed.items[0]
        );
    }

    /// Review-rework round 2 (t360.20.30, MAJOR): the top-ranked overall
    /// candidate is not necessarily a valid `refines:` target —
    /// `refines_edge_valid` (`src/trace/engine.rs`) requires the target to be
    /// left-side and at a strictly shallower level than the child. A
    /// same-level-or-right-side top hit (here `ST-001`, a
    /// `system_test`/right-side item that happens to be the lexically closer
    /// match) must be skipped in favor of the highest-ranked candidate that
    /// *does* qualify (`REQ-001`, left-side `requirement`, level 1 <
    /// `basic_spec`'s level 2) — reproduced against the exact scenario from
    /// the reviewer's real-binary repro (a `standard` project with an
    /// unrelated REQ-001 and a lexically closer ST-001; saving the old
    /// `- refines: ST-001` suggestion produced `invalid_link` from the
    /// engine).
    #[test]
    fn refines_skips_candidates_that_are_not_valid_upper_layer_targets() {
        let (_tmp, handoff) = setup();
        let config = "[project]\nname = \"t\"\n\n[trace]\nprofile = \"standard\"\n";
        std::fs::write(handoff.join("config.toml"), config).unwrap();

        let req_doc = layer_doc_with_items(
            "doc-1",
            "req-doc",
            "requirement",
            &[],
            vec![sub_item(
                "REQ-001",
                "Something unrelated",
                Some("requirement"),
            )],
        );
        let st_doc = layer_doc_with_items(
            "doc-2",
            "st-doc",
            "system_test",
            &[],
            vec![sub_item(
                "ST-001",
                "Audit log retention check",
                Some("system_test"),
            )],
        );
        write_doc(&handoff, &req_doc).unwrap();
        write_doc(&handoff, &st_doc).unwrap();

        let c = ctx(handoff.clone());
        let out: Value = serde_json::from_str(
            &handle_trace_propose(&c, &json!({"title": "Audit log retention"})).unwrap(),
        )
        .unwrap();

        // Sanity check on the repro itself: the lexically closer ST-001 does
        // outrank REQ-001 in `candidates` (otherwise this test would not
        // exercise the bug at all).
        assert_eq!(out["candidates"][0]["id"], "ST-001", "{out}");

        let markdown = out["proposal"]["markdown"].as_str().unwrap();
        assert!(markdown.contains("- refines: REQ-001"), "{markdown}");
        assert!(!markdown.contains("- refines: ST-001"), "{markdown}");

        // The suggested link must actually be accepted as valid once parsed
        // back, not merely "look right" in the rendered text.
        let registry = LayerRegistry::build(&[]);
        let prefix_table = default_prefix_table(&registry, &Default::default());
        let body = format!("# Spec\n\n{markdown}\n");
        let parsed = parse_layer_body(&body, Some("basic_spec"), &prefix_table);
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        assert_eq!(parsed.items[0].attrs.refines, vec!["REQ-001".to_string()]);
    }

    /// Review-rework round 2 (t360.20.30, MAJOR): when *no* candidate is a
    /// valid upper-layer `refines:` target (here the only candidate at all is
    /// `ST-001`, right-side), the template falls back to the same empty-line
    /// and `warnings` guidance the zero-candidates case already uses — never
    /// a candidate the engine would reject.
    #[test]
    fn refines_leaves_an_empty_line_when_no_candidate_is_a_valid_upper_layer_target() {
        let (_tmp, handoff) = setup();
        let config = "[project]\nname = \"t\"\n\n[trace]\nprofile = \"standard\"\n";
        std::fs::write(handoff.join("config.toml"), config).unwrap();

        let st_doc = layer_doc_with_items(
            "doc-2",
            "st-doc",
            "system_test",
            &[],
            vec![sub_item(
                "ST-001",
                "Audit log retention check",
                Some("system_test"),
            )],
        );
        write_doc(&handoff, &st_doc).unwrap();

        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_trace_propose(&c, &json!({"title": "Audit log retention"})).unwrap(),
        )
        .unwrap();
        // Sanity check: a candidate does exist, it's just not usable as a
        // refines target.
        assert_eq!(out["candidates"][0]["id"], "ST-001", "{out}");

        let markdown = out["proposal"]["markdown"].as_str().unwrap();
        assert!(
            markdown.contains("- refines: \n") || markdown.contains("- refines:\n"),
            "{markdown}"
        );
        assert!(
            out["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w.as_str().unwrap().contains("refines")),
            "{out}"
        );
    }

    /// t360.20.31 (M2-S7 reviewer finding): a small caller-supplied `limit`
    /// must not starve the `refines:` suggestion of a valid candidate that
    /// exists but ranks just outside the `limit`-capped `candidates` field —
    /// same fixture as `refines_skips_candidates_that_are_not_valid_upper_layer_targets`
    /// (lexically closer, invalid-layer `ST-001` vs. the valid but less
    /// similar `REQ-001`), but with `limit: 1` so only `ST-001` is visible in
    /// `candidates`. The old implementation only ever looked inside
    /// `candidates` for a `refines:` pick, so with `ST-001` alone (invalid
    /// layer) it fell back to the empty-line-plus-warning case even though a
    /// valid `REQ-001` existed in the corpus. The fix searches the larger,
    /// non-`limit`-capped ranked pool when nothing within `candidates`
    /// qualifies.
    #[test]
    fn refines_search_falls_back_beyond_the_limit_cap_when_nothing_within_it_is_valid() {
        let (_tmp, handoff) = setup();
        let config = "[project]\nname = \"t\"\n\n[trace]\nprofile = \"standard\"\n";
        std::fs::write(handoff.join("config.toml"), config).unwrap();

        let req_doc = layer_doc_with_items(
            "doc-1",
            "req-doc",
            "requirement",
            &[],
            vec![sub_item(
                "REQ-001",
                "Something unrelated",
                Some("requirement"),
            )],
        );
        let st_doc = layer_doc_with_items(
            "doc-2",
            "st-doc",
            "system_test",
            &[],
            vec![sub_item(
                "ST-001",
                "Audit log retention check",
                Some("system_test"),
            )],
        );
        write_doc(&handoff, &req_doc).unwrap();
        write_doc(&handoff, &st_doc).unwrap();

        let c = ctx(handoff.clone());
        let out: Value = serde_json::from_str(
            &handle_trace_propose(&c, &json!({"title": "Audit log retention", "limit": 1}))
                .unwrap(),
        )
        .unwrap();

        // `limit: 1` really does cap `candidates` to the lexically closer
        // (but refines-invalid) ST-001 — otherwise this test would not
        // exercise the bug at all.
        let candidates = out["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 1, "{out}");
        assert_eq!(candidates[0]["id"], "ST-001", "{out}");

        let markdown = out["proposal"]["markdown"].as_str().unwrap();
        assert!(markdown.contains("- refines: REQ-001"), "{markdown}");
        assert!(!markdown.contains("- refines: ST-001"), "{markdown}");
    }

    /// `minimal`'s only left layer (`requirement`) is trivially its own
    /// top-most left layer — no `refines:` line at all (unlike `standard`'s
    /// `basic_spec` above), matching the pre-existing minimal-profile
    /// template shape exactly.
    #[test]
    fn minimal_profile_template_has_no_refines_line() {
        let (_tmp, handoff) = setup();
        let config = "[project]\nname = \"t\"\n\n[trace]\nprofile = \"minimal\"\n";
        std::fs::write(handoff.join("config.toml"), config).unwrap();

        let c = ctx(handoff);
        let out: Value = serde_json::from_str(
            &handle_trace_propose(&c, &json!({"title": "Account lockout"})).unwrap(),
        )
        .unwrap();
        let markdown = out["proposal"]["markdown"].as_str().unwrap();
        assert!(!markdown.contains("refines"), "{markdown}");
    }

    /// M2-S6 finding, item 3: the *target document's own* `trace_profile`
    /// override decides the template's `implicit_acceptance` (wiki/260
    /// §2.1/§2.5 手順3's "文書の上書き ＞ プロジェクト既定" priority,
    /// `resolve_doc_implicit_acceptance`'s own rule) — not just the project
    /// default profile. The target document is a `basic_spec` document
    /// (matching the project-level `standard` profile's own deepest-left
    /// layer choice; this override only changes `implicit_acceptance`, never
    /// the layer/profile-set selection itself — out of scope per this task's
    /// instructions) whose own `trace_profile` is `minimal`
    /// (`implicit_acceptance: true`), so the proposed template is a single
    /// item with an inline 受入基準 block — not a `basic_spec`/`system_test`
    /// pair — even though the project default alone would have produced the
    /// pair.
    #[test]
    fn target_docs_trace_profile_override_decides_implicit_acceptance() {
        let (_tmp, handoff) = setup();
        std::fs::write(
            handoff.join("config.toml"),
            "[project]\nname = \"t\"\n\n[trace]\nprofile = \"standard\"\n",
        )
        .unwrap();
        let mut doc =
            layer_doc_with_items("doc-1", "auth-spec", "basic_spec", &["src/auth/"], vec![]);
        doc.trace_profile = Some("minimal".to_string());
        write_doc(&handoff, &doc).unwrap();

        let tasks_dir = handoff.join("tasks");
        let task_dir = tasks_dir.join("t1-auth");
        std::fs::create_dir_all(&task_dir).unwrap();
        let data = crate::storage::tasks::TaskData {
            id: "t1".to_string(),
            title: "Add account lockout".to_string(),
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
            scope_paths: vec!["src/auth/login.rs".to_string()],
            extra: Default::default(),
        };
        crate::storage::tasks::write_task(&task_dir, "todo", &data).unwrap();

        let c = ctx(handoff);
        let out: Value =
            serde_json::from_str(&handle_trace_propose(&c, &json!({"task_id": "t1"})).unwrap())
                .unwrap();
        assert_eq!(out["proposal"]["doc"], "auth-spec", "{out}");
        // The layer-set-driving `profile` label stays the project-resolved
        // one (`standard`) — only `implicit_acceptance` is overridden.
        assert_eq!(out["proposal"]["profile"], "standard", "{out}");
        let next_ids = out["proposal"]["next_ids"].as_array().unwrap();
        assert_eq!(next_ids.len(), 1, "{out}");
        assert!(next_ids[0].as_str().unwrap().starts_with("SPEC-"), "{out}");
        let markdown = out["proposal"]["markdown"].as_str().unwrap();
        assert!(markdown.contains("受入基準"), "{markdown}");
        assert!(
            out["warnings"].as_array().unwrap().iter().any(|w| w
                .as_str()
                .unwrap()
                .contains("trace_profile")
                && w.as_str().unwrap().contains("implicit_acceptance")),
            "{out}"
        );
    }

    /// When a document's own `trace_profile` names a profile that cannot be
    /// resolved (unknown name), the project-level `implicit_acceptance`
    /// (from the already-resolved tiers) is kept rather than silently
    /// defaulting to `false` — the resolution failure is reported, not
    /// swallowed.
    #[test]
    fn unresolvable_target_doc_trace_profile_keeps_the_project_level_implicit_acceptance() {
        let (_tmp, handoff) = setup();
        std::fs::write(
            handoff.join("config.toml"),
            "[project]\nname = \"t\"\n\n[trace]\nprofile = \"minimal\"\n",
        )
        .unwrap();
        let mut doc =
            layer_doc_with_items("doc-1", "req-doc", "requirement", &["src/auth/"], vec![]);
        doc.trace_profile = Some("nonexistent".to_string());
        write_doc(&handoff, &doc).unwrap();

        let tasks_dir = handoff.join("tasks");
        let task_dir = tasks_dir.join("t1-auth");
        std::fs::create_dir_all(&task_dir).unwrap();
        let data = crate::storage::tasks::TaskData {
            id: "t1".to_string(),
            title: "Add account lockout".to_string(),
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
            scope_paths: vec!["src/auth/login.rs".to_string()],
            extra: Default::default(),
        };
        crate::storage::tasks::write_task(&task_dir, "todo", &data).unwrap();

        let c = ctx(handoff);
        let out: Value =
            serde_json::from_str(&handle_trace_propose(&c, &json!({"task_id": "t1"})).unwrap())
                .unwrap();
        // minimal's own implicit_acceptance (true) is kept — the
        // unresolvable doc-level override must not silently coerce it to
        // `false`.
        let next_ids = out["proposal"]["next_ids"].as_array().unwrap();
        assert_eq!(next_ids.len(), 1, "{out}");
        let markdown = out["proposal"]["markdown"].as_str().unwrap();
        assert!(markdown.contains("受入基準"), "{markdown}");
        assert!(
            out["warnings"].as_array().unwrap().iter().any(|w| w
                .as_str()
                .unwrap()
                .contains("'nonexistent' could not be resolved")),
            "{out}"
        );
    }

    #[test]
    fn next_number_for_prefix_starts_at_one_when_nothing_exists() {
        assert_eq!(next_number_for_prefix(&HashSet::new(), "REQ"), 1);
    }

    /// §6 perf fix: `hybrid_score` (a precomputed-query-embedding
    /// reimplementation) must produce the same value `lexsim::hybrid_jaccard`
    /// itself would for the same inputs — otherwise the optimization would
    /// silently change candidate ranking, not just its speed.
    #[test]
    fn hybrid_score_matches_lexsim_hybrid_jaccard_for_the_same_inputs() {
        let model = semantic_model();
        let query = "Account lockout after failed logins";
        let candidate = "Account lockout policy";
        let expected = lexsim::hybrid_jaccard(query, candidate, model, HYBRID_ALPHA);
        let query_embedding = model.embed(query).ok();
        let actual = hybrid_score(
            query_embedding.as_deref(),
            query,
            candidate,
            model,
            HYBRID_ALPHA,
        );
        assert!(
            (expected - actual).abs() < 1e-6,
            "expected {expected}, got {actual}"
        );
    }
}
