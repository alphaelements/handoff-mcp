use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;

use super::{HandlerContext, StructuredWarning, Warning};
use crate::storage::agents::{
    generate_agent_id, read_agent, write_agent, AgentRecord, AgentStatus,
};
use crate::storage::config::read_config;
use crate::storage::referrals::read_referral_summaries;
use crate::storage::sessions::{
    activate_open_sessions, activate_session_by_id, read_active_sessions,
    read_latest_closed_session, read_open_sessions, read_paused_sessions,
    resume_paused_session_by_id,
};
use crate::storage::tasks::{build_task_index, build_task_index_with_expiry, TaskIndex};

/// Maximum depth (relative to the base project dir) scanned for nested
/// `.handoff/` child projects.
const MAX_CHILD_SCAN_DEPTH: usize = 5;

/// Directory names skipped while scanning for child projects.
const DEFAULT_SCAN_EXCLUDES: &[&str] = &["node_modules", ".git", "target", "dist", ".next"];

/// `_trace_report.json` older than this is flagged `stale: true` in
/// `suspect_summary` (DS-P4-006) — the file only refreshes when a trace
/// report/suspect tool runs, so an old mtime means the summary may lag the
/// current documents.
const SUSPECT_SUMMARY_STALE_AFTER: Duration = Duration::from_secs(60 * 60);

/// Cap on `suspect_summary.items`, keeping `load_context` small on projects
/// with many suspects; `total`/`by_kind` always count every suspect.
const SUSPECT_SUMMARY_MAX_ITEMS: usize = 20;

/// Like `init`, `load_context` must tolerate a project that has no
/// `.handoff/` yet (it reports `not_initialized` instead of erroring), so it
/// checks `ctx.handoff_dir.exists()` itself rather than relying on the
/// dispatch layer to have pre-validated it via `ensure_handoff_exists`.
pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let project_dir = &ctx.project_dir;

    let hdir = &ctx.handoff_dir;
    if !hdir.exists() {
        let result = serde_json::json!({
            "status": "not_initialized",
            "message": format!(
                "No .handoff/ directory found in {}. Run handoff_init to set up handoff tracking.",
                project_dir.display()
            )
        });
        return serde_json::to_string_pretty(&result).context("serialize");
    }

    let handoff = hdir;
    let sessions_dir = handoff.join("sessions");
    let tasks_dir = handoff.join("tasks");
    let config_path = handoff.join("config.toml");

    // Lazy scan (spec 3.3.5, 7.2) is now folded into
    // `build_task_index_with_expiry` itself (P-M6, wiki/240-performance-
    // design.md §3 C6 / §4): the tree built below already reflects any lease
    // it reclaims along the way, in the same single pass, instead of a
    // separate `scan_expired_leases` walk before it. Reclaiming here is
    // limited to *this* project's own tree, per wiki/190's "Lazy scan の対象
    // 操作" allowlist — `discover_child_project_info`'s cross-project scan
    // below uses the read-only `build_task_index` instead (rework round 2:
    // must not mutate another project's task files).
    let config = if config_path.exists() {
        read_config(&config_path)?
    } else {
        anyhow::bail!("config.toml not found");
    };

    let target_session_id = arguments.get("session_id").and_then(|v| v.as_str());

    let active_sessions = read_active_sessions(&sessions_dir)?;
    let sessions = read_open_sessions(&sessions_dir)?;
    let paused_sessions = read_paused_sessions(&sessions_dir)?;

    let multi_session = config.settings.multi_session;

    let selected_session = if let Some(sid) = target_session_id {
        let already_active = active_sessions
            .iter()
            .any(|s| s.id.as_deref().is_some_and(|id| id == sid));
        if already_active {
            active_sessions
                .into_iter()
                .find(|s| s.id.as_deref().is_some_and(|id| id == sid))
        } else if !multi_session && !active_sessions.is_empty() {
            let active_ids: Vec<String> = active_sessions
                .iter()
                .filter_map(|s| s.id.clone())
                .collect();
            anyhow::bail!(
                "Cannot activate session '{sid}': another session is already active ({}).\n\
                 Use save_context with close_session_id or pause_session_id to \
                 close/pause the active session first, or enable multi_session in config.",
                active_ids.join(", ")
            );
        } else if activate_session_by_id(&sessions_dir, sid)?.is_some() {
            sessions
                .into_iter()
                .find(|s| s.id.as_deref().is_some_and(|id| id == sid))
        } else if resume_paused_session_by_id(&sessions_dir, sid)?.is_some() {
            paused_sessions
                .into_iter()
                .find(|s| s.id.as_deref().is_some_and(|id| id == sid))
        } else {
            None
        }
    } else if !active_sessions.is_empty() {
        active_sessions.into_iter().last()
    } else if sessions.len() > 1 {
        let open_ids: Vec<String> = sessions.iter().filter_map(|s| s.id.clone()).collect();
        anyhow::bail!(
            "Multiple open sessions found ({}).\n\
             Specify session_id to choose one, or use save_context to \
             close/pause the others first.",
            open_ids.join(", ")
        );
    } else {
        activate_open_sessions(&sessions_dir)?;
        sessions.into_iter().last()
    };

    let (task_tree, task_summary, _expired_ids) =
        build_task_index_with_expiry(&tasks_dir, config.settings.done_task_limit)?;

    let session_id_for_agent = selected_session.as_ref().and_then(|s| s.id.clone());
    let agent = register_agent(handoff, project_dir, session_id_for_agent)?;
    crate::mcp::router::set_agent_id(agent.agent_id.clone());
    // Best-effort GC of long-disconnected agent records; never let a GC
    // failure fail the whole load_context call.
    let _ = crate::storage::agents::gc_agents(handoff);

    let mut result = serde_json::json!({
        "project": config.project.name,
        "task_tree": task_tree,
        "task_summary": task_summary,
        "agent_id": agent.agent_id,
        "claimed_tasks": agent.claimed_tasks,
    });

    // Accumulate every independent warning condition and join them, so one
    // condition (e.g. session-not-found) can never silently clobber another
    // (e.g. version mismatch) when both occur on the same call.
    let mut warnings: Vec<Warning> = Vec::new();

    if let Some(warning) = version_mismatch_warning(handoff) {
        warnings.push(warning.into());
    }

    if selected_session.is_none() {
        if let Some(sid) = target_session_id {
            warnings.push(format!("session_id '{sid}' not found among open sessions").into());
        }
    }

    let previous_closed_session = read_latest_closed_session(&sessions_dir)?;
    if let Some(prev) = previous_closed_session.as_ref() {
        if let Some(warning) = incomplete_tasks_warning(&prev.related_task_ids, &task_tree) {
            warnings.push(warning.into());
        }
    }

    if !warnings.is_empty() {
        result["warnings"] = serde_json::to_value(&warnings).context("serialize warnings")?;
    }

    if let Some(ref session) = selected_session {
        result["last_session"] = serde_json::json!({
            "ended_at": session.ended_at,
            "summary": session.summary,
            "branch": session.branch,
            "commit": session.commit,
        });

        if let Some(ref id) = session.id {
            result["session_id"] = serde_json::json!(id);
        }

        let session_val = serde_json::to_value(session).unwrap_or_default();

        for key in [
            "decisions",
            "blockers",
            "checklist",
            "handoff_notes",
            "references",
            "context_pointers",
        ] {
            if let Some(val) = session_val.get(key) {
                if val.as_array().is_some_and(|a| !a.is_empty()) {
                    result[key] = val.clone();
                }
            }
        }

        if let Some(env) = session_val.get("environment") {
            if !env.is_null() {
                result["environment"] = env.clone();
            }
        }
    }

    if let Some(prev) = previous_closed_session {
        let prev_val = serde_json::to_value(&prev).unwrap_or_default();
        let mut prev_obj = serde_json::json!({
            "summary": prev.summary,
            "ended_at": prev.ended_at,
            "branch": prev.branch,
            "commit": prev.commit,
        });
        if let Some(ref id) = prev.id {
            prev_obj["id"] = serde_json::json!(id);
        }
        for key in [
            "decisions",
            "handoff_notes",
            "context_pointers",
            "checklist",
            "references",
            "blockers",
        ] {
            if let Some(val) = prev_val.get(key) {
                if val.as_array().is_some_and(|a| !a.is_empty()) {
                    prev_obj[key] = val.clone();
                }
            }
        }
        if let Some(env) = prev_val.get("environment") {
            if !env.is_null() {
                prev_obj["environment"] = env.clone();
            }
        }
        result["previous_session"] = prev_obj;
    }

    let notes_sources: Vec<&Value> = [
        result.get("handoff_notes"),
        result
            .get("previous_session")
            .and_then(|ps| ps.get("handoff_notes")),
    ]
    .into_iter()
    .flatten()
    .collect();

    let suggestions: Vec<&str> = notes_sources
        .iter()
        .filter_map(|v| v.as_array())
        .flatten()
        .filter(|n| {
            n.get("category")
                .and_then(|c| c.as_str())
                .is_some_and(|c| c == "suggestion")
        })
        .filter_map(|n| n.get("note").and_then(|v| v.as_str()))
        .collect();

    if !suggestions.is_empty() {
        result["next_actions"] = serde_json::json!(suggestions);
    }

    if !config.settings.context_files.is_empty() {
        result["suggested_reads"] = serde_json::to_value(&config.settings.context_files)?;
    }

    let referrals_dir = handoff.join("referrals");
    let open_referrals = read_referral_summaries(&referrals_dir, Some("open"))?;
    if !open_referrals.is_empty() {
        result["referrals"] = serde_json::to_value(&open_referrals)?;
    }

    let current_open = read_open_sessions(&sessions_dir)?;
    if !current_open.is_empty() {
        let summaries: Vec<Value> = current_open.iter().map(session_summary_json).collect();
        result["open_sessions"] = serde_json::json!(summaries);
    }

    let current_paused = read_paused_sessions(&sessions_dir)?;
    if !current_paused.is_empty() {
        let summaries: Vec<Value> = current_paused.iter().map(session_summary_json).collect();
        result["paused_sessions"] = serde_json::json!(summaries);
    }

    let current_active = read_active_sessions(&sessions_dir)?;
    if current_active.len() > 1 {
        let summaries: Vec<Value> = current_active.iter().map(session_summary_json).collect();
        result["active_sessions"] = serde_json::json!(summaries);
    }

    if current_active.is_empty() {
        let mut guidance = serde_json::json!({
            "action": "create_session",
            "message": "No active session. Before starting work, call handoff_save_context with session_status='active' to establish a session. Include inherited context (decisions, context_pointers, references) from the previous session so your work survives interruptions."
        });
        if let Some(prev) = result.get("previous_session") {
            let mut suggested = serde_json::json!({});
            if let Some(summary) = prev.get("summary").and_then(|v| v.as_str()) {
                suggested["summary"] = serde_json::json!(format!("Continuing: {summary}"));
            }
            for key in ["decisions", "context_pointers", "references"] {
                if let Some(val) = prev.get(key) {
                    if val.as_array().is_some_and(|a| !a.is_empty()) {
                        suggested[key] = val.clone();
                    }
                }
            }
            if let Some(task_ids) = collect_active_task_ids(&task_tree) {
                suggested["related_task_ids"] = serde_json::json!(task_ids);
            }
            guidance["suggested_fields"] = suggested;
        }
        result["session_guidance"] = guidance;
    } else if current_active.len() > 1 && target_session_id.is_none() {
        let summaries: Vec<Value> = current_active.iter().map(session_summary_json).collect();
        result["session_guidance"] = serde_json::json!({
            "action": "select_session",
            "message": "Multiple active sessions. Use session_id to specify which to work with, or create a new session.",
            "active_sessions": summaries
        });
    }

    // Always include child_projects (empty array if none).
    let child_projects = discover_child_project_info(project_dir);
    result["child_projects"] = serde_json::json!(child_projects);

    // t378.4 (M4): a lightweight health summary read from the M5-written
    // derived files (`_trace_report.json`/`_requirements_summary.json`) —
    // never a fresh `rebuild_trace_graph`/`aggregate_requirements` call,
    // which would blow this tool's 200ms budget (wiki/240-performance-
    // design.md measured those at 107-271ms at L scale). See
    // `trace_health_summary`/`requirements_health_summary`/
    // `docs_health_summary`'s own doc comments for the exact read shape.
    let trace_health = trace_health_summary(handoff);
    let requirements_health = requirements_health_summary(handoff);
    let docs_health = docs_health_summary(handoff);
    if let Some(message) =
        health_guidance_message(&trace_health, &requirements_health, &docs_health)
    {
        let guidance = result
            .get_mut("session_guidance")
            .and_then(|g| g.as_object_mut());
        match guidance {
            Some(obj) => {
                let combined = match obj.get("message").and_then(|m| m.as_str()) {
                    Some(existing) => format!("{existing} {message}"),
                    None => message,
                };
                obj.insert("message".to_string(), serde_json::json!(combined));
            }
            None => {
                result["session_guidance"] = serde_json::json!({ "message": message });
            }
        }
    }
    result["trace_health"] = trace_health;
    result["suspect_summary"] = suspect_summary(handoff, SystemTime::now());
    result["requirements_health"] = requirements_health;
    result["docs_health"] = docs_health;

    serde_json::to_string_pretty(&result).context("Failed to serialize context")
}

/// Reads `.handoff/docs/_trace_report.json` (written by
/// [`crate::mcp::handlers::trace::write_trace_report`], M5/t378.5) with a
/// plain `std::fs::read` + `serde_json::from_slice::<Value>` — deliberately
/// never `rebuild_trace_graph` (measured 107-180ms at L scale, wiki/240-
/// performance-design.md §5-5), since `load_context` has its own 200ms
/// budget to stay under regardless of whichever other tool last refreshed
/// this file.
///
/// `warnings`/`coverage` are copied through verbatim from whatever is on
/// disk; a file with no `warnings` key at all (every file written before
/// M5) reads as an empty array rather than failing — `Value`-based reading
/// has no schema to reject against, so backward compatibility here is
/// "whatever key is present is used, whatever is absent defaults to empty".
fn trace_health_summary(handoff: &Path) -> Value {
    let path = handoff.join("docs").join("_trace_report.json");
    let Some(persisted) = read_json_file(&path) else {
        return serde_json::json!({ "has_data": false, "warnings": [] });
    };
    let warnings = persisted
        .get("warnings")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let mut out = serde_json::json!({ "has_data": true, "warnings": warnings });
    if let Some(coverage) = persisted.get("coverage") {
        out["coverage"] = coverage.clone();
    }
    out
}

/// t391.3 (DS-P4-006): suspect summary read from the same persisted
/// `_trace_report.json` as [`trace_health_summary`] (never a graph rebuild —
/// see that function's budget note). Each `items[].suspect` entry becomes one
/// summary item; `Value::Null` when there are no suspects (or no report yet).
///
/// `stale` is mtime-based, since the file carries no capture timestamp: `true`
/// when `now` is more than [`SUSPECT_SUMMARY_STALE_AFTER`] past the file's
/// mtime. An mtime the filesystem cannot report is treated as stale (unknown
/// freshness must not read as fresh); an mtime in the future (clock skew) is
/// age zero, i.e. fresh. Informational only — the suspect data is returned
/// either way.
fn suspect_summary(handoff: &Path, now: SystemTime) -> Value {
    let path = handoff.join("docs").join("_trace_report.json");
    let Some(persisted) = read_json_file(&path) else {
        return Value::Null;
    };
    let (mut link, mut task, mut result) = (0usize, 0usize, 0usize);
    let mut items: Vec<Value> = Vec::new();
    let report_items = persisted
        .get("items")
        .and_then(|i| i.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();
    for item in report_items {
        let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let suspects = item
            .get("suspect")
            .and_then(|v| v.as_array())
            .map(Vec::as_slice)
            .unwrap_or_default();
        for s in suspects {
            let entry = match s.get("kind").and_then(|k| k.as_str()) {
                Some("link") => {
                    link += 1;
                    serde_json::json!({
                        "kind": "link",
                        "item": id,
                        "upstream": s.get("upstream").cloned().unwrap_or(Value::Null),
                        "reason": "def_hash changed",
                    })
                }
                Some("task") => {
                    task += 1;
                    serde_json::json!({
                        "kind": "task",
                        "task_id": s.get("task").cloned().unwrap_or(Value::Null),
                        "stable_id": id,
                        "reason": "baseline_hash changed",
                    })
                }
                Some("result") => {
                    result += 1;
                    serde_json::json!({
                        "kind": "result",
                        "item": id,
                        "reason": "definition changed since the last recorded result",
                    })
                }
                _ => continue,
            };
            items.push(entry);
        }
    }
    let total = link + task + result;
    if total == 0 {
        return Value::Null;
    }
    let stale = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .map(|mtime| {
            now.duration_since(mtime)
                .is_ok_and(|age| age > SUSPECT_SUMMARY_STALE_AFTER)
        })
        .unwrap_or(true);
    let truncated = items.len() > SUSPECT_SUMMARY_MAX_ITEMS;
    items.truncate(SUSPECT_SUMMARY_MAX_ITEMS);
    let mut out = serde_json::json!({
        "total": total,
        "by_kind": { "link": link, "task": task, "result": result },
        "items": items,
        "stale": stale,
    });
    if truncated {
        out["truncated"] = Value::Bool(true);
    }
    out
}

/// Reads `.handoff/docs/_requirements_summary.json` (written by
/// [`crate::mcp::handlers::docs::write_requirements_summary`], M5/t378.5) —
/// same lightweight `fs::read` + `from_slice::<Value>` discipline as
/// [`trace_health_summary`], never `aggregate_requirements`.
fn requirements_health_summary(handoff: &Path) -> Value {
    let path = handoff.join("docs").join("_requirements_summary.json");
    let Some(persisted) = read_json_file(&path) else {
        return serde_json::json!({ "has_data": false, "warnings": [] });
    };
    let warnings = persisted
        .get("warnings")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let total = persisted.get("total").cloned().unwrap_or(Value::Null);
    let by_status = persisted
        .get("by_status")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    serde_json::json!({
        "has_data": true,
        "warnings": warnings,
        "total": total,
        "by_status": by_status,
    })
}

/// Reads `.handoff/docs/_requirements_summary.json` — the same file
/// [`requirements_health_summary`] reads — but surfaces the
/// *document*-level facet of it: the DIAG-R001/DIAG-R002 diagnostics persisted
/// there are fundamentally about documents being excluded from requirement
/// aggregation (no `layer` set, no verification matrix, a `SubItem` with no
/// `stable_id`), not about requirement-item stats, so `docs_health` repeats
/// the same `warnings` array under its own name alongside `total_docs`
/// (`inputs.docs_count`, the total document count the summary was computed
/// over) rather than `requirements_health`'s `total`/`by_status`. There is
/// no separate `_docs_*.json` file (the diagnostics-improvement-plan design
/// doc, §M4/M5, only ever defines two persisted files); `docs_health` is a
/// view over `_requirements_summary.json`, not a third file.
fn docs_health_summary(handoff: &Path) -> Value {
    let path = handoff.join("docs").join("_requirements_summary.json");
    let Some(persisted) = read_json_file(&path) else {
        return serde_json::json!({ "has_data": false, "warnings": [] });
    };
    let warnings = persisted
        .get("warnings")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let total_docs = persisted
        .get("inputs")
        .and_then(|i| i.get("docs_count"))
        .cloned()
        .unwrap_or(Value::Null);
    serde_json::json!({
        "has_data": true,
        "warnings": warnings,
        "total_docs": total_docs,
    })
}

/// Shared by both health summaries above: a plain read + parse, `None` on
/// any failure (file absent, unreadable, or not valid JSON) — every failure
/// mode collapses to "no data yet", never an error, since a missing/stale
/// derived file is an expected, normal state (e.g. `handoff_trace_report`
/// has simply never run yet in this project).
fn read_json_file(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// D4 (session context, M4 design decision): when any health summary
/// carries an "error"/"warning"-severity [`crate::mcp::handlers::StructuredWarning`],
/// fold one short mention into `session_guidance.message` so a session
/// starting up notices project-health issues (unconfigured trace layers,
/// documents with no layer/matrix) without having to separately call
/// `handoff_trace_report`/`handoff_doc_req_status`. A plain-string warning
/// entry (the untagged `Warning::Plain` variant) has no `severity` to check,
/// so only object-shaped entries can trigger this — matching every other
/// `code`-based branch in this codebase (e.g. `trace.rs`'s `diag_codes`
/// test helper).
fn health_guidance_message(
    trace_health: &Value,
    requirements_health: &Value,
    docs_health: &Value,
) -> Option<String> {
    let has_actionable_warning = |health: &Value| {
        health["warnings"].as_array().is_some_and(|warnings| {
            warnings.iter().any(|w| {
                w.get("severity")
                    .and_then(|s| s.as_str())
                    .is_some_and(|s| s == "error" || s == "warning")
            })
        })
    };
    if has_actionable_warning(trace_health)
        || has_actionable_warning(requirements_health)
        || has_actionable_warning(docs_health)
    {
        Some(
            "プロジェクトの健全性に注意が必要です — docs_health/trace_health/\
             requirements_health の warnings を確認してください。"
                .to_string(),
        )
    } else {
        None
    }
}

/// Register (or refresh) this process's agent record under
/// `.handoff/agents/<agent-id>.json` (spec §7.2).
///
/// The agent id itself is a stable per-process/CLI-session identity (see
/// [`generate_agent_id`]), so a reconnecting agent — same `CLAUDE_SESSION_ID`,
/// new process — resolves to the *same* record: this updates that existing
/// record (preserving its `claimed_tasks` and `registered_at`) rather than
/// creating a fresh one that would forget in-flight claims.
fn register_agent(
    handoff: &Path,
    project_dir: &Path,
    session_id: Option<String>,
) -> Result<AgentRecord> {
    let agent_id = generate_agent_id();
    let now = Utc::now();
    // `capture_git_state` itself never errors (it falls back to "unknown"
    // per-field on git failures); treat that sentinel as "no branch info"
    // rather than storing the literal string "unknown" as a branch name.
    let branch = crate::storage::git::capture_git_state(project_dir)
        .ok()
        .map(|g| g.branch)
        .filter(|b| b != "unknown");

    let existing_record = read_agent(handoff, &agent_id)?;
    let is_new = existing_record.is_none();

    let record = if let Some(mut existing) = existing_record {
        existing.session_id = session_id.or(existing.session_id);
        existing.worktree = project_dir.to_path_buf();
        existing.branch = branch;
        existing.pid = Some(std::process::id());
        existing.last_heartbeat = now;
        existing.status = AgentStatus::Active;
        existing
    } else {
        AgentRecord {
            agent_id: agent_id.clone(),
            session_id,
            worktree: project_dir.to_path_buf(),
            branch,
            pid: Some(std::process::id()),
            registered_at: now,
            last_heartbeat: now,
            status: AgentStatus::Active,
            claimed_tasks: Vec::new(),
            metadata: Default::default(),
        }
    };

    write_agent(handoff, &record)?;

    // Only a genuinely new agent identity is worth an events.jsonl entry
    // (spec 3.6.3, 4.2 FR-2.5); a reconnect from the same CLAUDE_SESSION_ID
    // just refreshes the existing record and would otherwise flood the log
    // with one `agent.registered` per tool call.
    if is_new {
        let _ = crate::storage::events::append_event(
            handoff,
            crate::storage::events::EventRecord {
                ts: now.to_rfc3339(),
                event: "agent.registered".to_string(),
                task_id: None,
                agent_id: Some(agent_id),
                session_id: record.session_id.clone(),
                detail: None,
            },
        );
    }

    Ok(record)
}

/// Compare `.handoff/version` (written by `handoff_init`, spec §3.7) against
/// the running binary's `CARGO_PKG_VERSION`. Returns `None` when the marker
/// is absent (pre-existing `.handoff/` from before this feature, or a
/// version identical to this binary's) — the mismatch case is the only one
/// worth surfacing, since mixed versions sharing one `.handoff/` can
/// silently ignore each other's lock fields.
fn version_mismatch_warning(handoff: &Path) -> Option<String> {
    let version_path = handoff.join("version");
    let marker_version = std::fs::read_to_string(&version_path).ok()?;
    let marker_version = marker_version.trim();
    let binary_version = env!("CARGO_PKG_VERSION");

    if marker_version.is_empty() || marker_version == binary_version {
        return None;
    }

    Some(format!(
        "Warning: This handoff-mcp binary (v{binary_version}) differs from the .handoff/ \
         version marker (v{marker_version}). Mixed versions sharing the same .handoff/ may \
         cause lock fields to be silently ignored. Please update all instances to the same \
         version."
    ))
}

/// Warns when a task the previous (closed) session was working on is still
/// `in_progress` — the structural backstop for a session that ended without
/// completing Step 6 (done_criteria / status close-out) for its tasks.
///
/// Deliberately compares against the previous session's `related_task_ids`
/// rather than scanning `in_progress` task ages: an in_progress task that no
/// closed session ever claimed (e.g. work another session is doing right now)
/// is not evidence of a skipped close-out. Only `in_progress` counts —
/// `review`/`blocked` are deliberate resting states.
fn incomplete_tasks_warning(
    previous_related_task_ids: &[String],
    task_tree: &[TaskIndex],
) -> Option<StructuredWarning> {
    if previous_related_task_ids.is_empty() {
        return None;
    }
    let mut in_progress = Vec::new();
    collect_in_progress_ids(task_tree, &mut in_progress);
    let stale: Vec<String> = in_progress
        .into_iter()
        .filter(|id| previous_related_task_ids.contains(id))
        .collect();
    if stale.is_empty() {
        return None;
    }
    Some(StructuredWarning {
        severity: "warning".to_string(),
        code: "INCOMPLETE_TASKS".to_string(),
        message: format!(
            "{} task(s) from the previous session are still in progress: {}",
            stale.len(),
            stale.join(", ")
        ),
        fix_hint: Some("Review these tasks and mark them done or blocked.".to_string()),
        affected_doc_ids: vec![],
    })
}

fn collect_in_progress_ids(tasks: &[TaskIndex], ids: &mut Vec<String>) {
    for task in tasks {
        if task.status == "in_progress" {
            ids.push(task.id.clone());
        }
        collect_in_progress_ids(&task.children, ids);
    }
}

fn session_summary_json(s: &crate::storage::sessions::SessionData) -> Value {
    let mut obj = serde_json::json!({
        "id": s.id,
        "summary": s.summary,
        "ended_at": s.ended_at,
        "branch": s.branch,
    });
    if let Some(ref label) = s.label {
        obj["label"] = serde_json::json!(label);
    }
    if let Some(ref timeline) = s.timeline {
        obj["timeline"] = serde_json::json!(timeline);
    }
    obj
}

fn collect_active_task_ids(task_tree: &[TaskIndex]) -> Option<Vec<String>> {
    let mut ids = Vec::new();
    collect_active_ids_recursive(task_tree, &mut ids);
    if ids.is_empty() {
        None
    } else {
        Some(ids)
    }
}

fn collect_active_ids_recursive(tasks: &[TaskIndex], ids: &mut Vec<String>) {
    for task in tasks {
        if matches!(
            task.status.as_str(),
            "in_progress" | "blocked" | "todo" | "review"
        ) {
            ids.push(task.id.clone());
        }
        collect_active_ids_recursive(&task.children, ids);
    }
}

fn discover_child_project_info(base: &Path) -> Vec<Value> {
    let mut results = Vec::new();
    scan_for_children(base, 1, MAX_CHILD_SCAN_DEPTH, &mut results);
    results
}

fn scan_for_children(dir: &Path, depth: usize, max_depth: usize, results: &mut Vec<Value>) {
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
        let handoff_dir = path.join(".handoff");
        let config_path = handoff_dir.join("config.toml");
        if config_path.exists() {
            if let Ok(config) = read_config(&config_path) {
                let tasks_dir = handoff_dir.join("tasks");
                let (_, summary) = match build_task_index(&tasks_dir, 10) {
                    Ok(result) => result,
                    Err(_) => continue,
                };
                results.push(serde_json::json!({
                    "name": config.project.name,
                    "dir": path.to_string_lossy(),
                    "task_count": summary.total,
                    "status_summary": summary.by_status,
                }));
            }
        }
        scan_for_children(&path, depth + 1, max_depth, results);
    }
}

#[cfg(test)]
mod suspect_summary_tests {
    use super::*;
    use serde_json::json;

    fn write_report(handoff: &Path, items: Value) -> std::path::PathBuf {
        let docs = handoff.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        let path = docs.join("_trace_report.json");
        std::fs::write(&path, json!({ "items": items }).to_string()).unwrap();
        path
    }

    fn sample_items() -> Value {
        json!([
            { "id": "REQ-1", "suspect": [
                { "kind": "task", "task": "t1", "baseline_hash": "a", "current_hash": "b" },
                { "kind": "task", "task": "t2", "baseline_hash": "a", "current_hash": "b" },
            ]},
            { "id": "DS-1", "suspect": [
                { "kind": "link", "upstream": "REQ-1", "link_type": "refines" },
            ]},
            { "id": "CLEAN-1", "suspect": [] },
        ])
    }

    #[test]
    fn null_without_report() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(suspect_summary(tmp.path(), SystemTime::now()), Value::Null);
    }

    #[test]
    fn null_when_no_suspects() {
        let tmp = tempfile::tempdir().unwrap();
        write_report(tmp.path(), json!([{ "id": "REQ-1", "suspect": [] }]));
        assert_eq!(suspect_summary(tmp.path(), SystemTime::now()), Value::Null);
    }

    #[test]
    fn summarises_suspects_by_kind() {
        let tmp = tempfile::tempdir().unwrap();
        write_report(tmp.path(), sample_items());
        let out = suspect_summary(tmp.path(), SystemTime::now());
        assert_eq!(out["total"], 3);
        assert_eq!(out["by_kind"], json!({ "link": 1, "task": 2, "result": 0 }));
        assert_eq!(out["stale"], false);
        let items = out["items"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert!(items.contains(&json!({
            "kind": "task", "task_id": "t1", "stable_id": "REQ-1",
            "reason": "baseline_hash changed",
        })));
        assert!(items.contains(&json!({
            "kind": "link", "item": "DS-1", "upstream": "REQ-1",
            "reason": "def_hash changed",
        })));
        assert!(out.get("truncated").is_none());
    }

    #[test]
    fn stale_is_mtime_based() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_report(tmp.path(), sample_items());
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

        let just_inside = mtime + SUSPECT_SUMMARY_STALE_AFTER;
        assert_eq!(suspect_summary(tmp.path(), just_inside)["stale"], false);

        let past = mtime + SUSPECT_SUMMARY_STALE_AFTER + Duration::from_secs(1);
        let out = suspect_summary(tmp.path(), past);
        assert_eq!(out["stale"], true);
        // Stale data is still returned.
        assert_eq!(out["total"], 3);

        // Clock skew (mtime in the future) reads as fresh.
        let before = mtime - Duration::from_secs(10);
        assert_eq!(suspect_summary(tmp.path(), before)["stale"], false);
    }

    #[test]
    fn items_are_capped_but_total_is_not() {
        let tmp = tempfile::tempdir().unwrap();
        let many: Vec<Value> = (0..SUSPECT_SUMMARY_MAX_ITEMS + 5)
            .map(|i| json!({ "id": format!("R-{i}"), "suspect": [{ "kind": "result" }] }))
            .collect();
        write_report(tmp.path(), json!(many));
        let out = suspect_summary(tmp.path(), SystemTime::now());
        assert_eq!(out["total"], SUSPECT_SUMMARY_MAX_ITEMS + 5);
        assert_eq!(out["by_kind"]["result"], SUSPECT_SUMMARY_MAX_ITEMS + 5);
        assert_eq!(
            out["items"].as_array().unwrap().len(),
            SUSPECT_SUMMARY_MAX_ITEMS
        );
        assert_eq!(out["truncated"], true);
    }

    #[test]
    fn load_context_exposes_suspect_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().to_path_buf();
        let handoff = project.join(".handoff");
        crate::mcp::handlers::init::handle(
            &HandlerContext {
                agent_id: None,
                project_dir: project.clone(),
                handoff_dir: handoff.clone(),
            },
            &json!({ "project_name": "p" }),
        )
        .unwrap();
        let ctx = HandlerContext {
            agent_id: None,
            project_dir: project,
            handoff_dir: handoff.clone(),
        };
        let none: Value = serde_json::from_str(&handle(&ctx, &json!({})).unwrap()).unwrap();
        assert!(none["suspect_summary"].is_null());
        assert!(none.as_object().unwrap().contains_key("suspect_summary"));

        write_report(&handoff, sample_items());
        let some: Value = serde_json::from_str(&handle(&ctx, &json!({})).unwrap()).unwrap();
        assert_eq!(some["suspect_summary"]["total"], 3);
    }
}
