use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use serde_json::Value;

use super::{lineage, HandlerContext};
use crate::storage::config::read_config;
use crate::storage::git::capture_git_state;
use crate::storage::session_target::{
    own_active_group, ownerless_bulk_warning, resolve_session_target, Caller, Resolution, Target,
    TargetOp, EVENT_CLOSE_REFUSED, EVENT_FOREIGN_CLOSE,
};
use crate::storage::sessions::{
    closed_head_ids, find_active_primary_session_id, generate_session_id, prune_closed_sessions,
    read_session_entries, transition_session_checked, update_session_at, write_session_with_status,
    SeenSession, SessionData, SessionEntry, SessionWriteError, WriteMode,
};

pub fn handle(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let project_dir = &ctx.project_dir;
    let handoff = &ctx.handoff_dir;
    let sessions_dir = handoff.join("sessions");

    let close_id = arguments.get("close_session_id").and_then(|v| v.as_str());
    let pause_id = arguments.get("pause_session_id").and_then(|v| v.as_str());
    let pause_all = arguments
        .get("pause_active")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let is_pause_only = arguments
        .get("pause_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let summary_opt = arguments.get("summary").and_then(|v| v.as_str());

    if !is_pause_only && summary_opt.is_none() {
        anyhow::bail!("'summary' is required (unless pause_only=true)");
    }

    let summary = summary_opt.unwrap_or("");

    let force = arguments
        .get("force_other_owner")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let reason = arguments
        .get("reason")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|r| !r.is_empty());
    if force && reason.is_none() {
        anyhow::bail!(
            "force_other_owner requires a non-empty 'reason' (it is recorded in the event log)"
        );
    }

    // This call's worktree identity. Together with `agent_id` it defines
    // "my" sessions for every implicit choice below (see `session_target`),
    // regardless of `session_status`.
    let call_worktree = project_dir.to_string_lossy().to_string();
    let caller = Caller {
        agent_id: ctx.agent_id.as_deref(),
        worktree: &call_worktree,
    };
    let entries = read_session_entries(&sessions_dir)?;
    // Warnings produced by the ownership rules, appended to the response.
    let mut notes: Vec<String> = Vec::new();

    let mut total_paused = 0usize;
    if let Some(id) = pause_id {
        match resolve_session_target(&entries, &caller, Some(id), TargetOp::Pause, force) {
            Resolution::Target(t) => {
                if let Some(entry) = entry_for(&entries, &t.id) {
                    transition_session_checked(&sessions_dir, &SeenSession::of(entry), "paused")
                        .map_err(|e| write_refusal(ctx, e, "pause"))?;
                    total_paused = 1;
                    record_ownership_event(ctx, &t, reason);
                    notes.extend(t.warnings);
                }
            }
            Resolution::Refused { id, owner } => {
                // `pause_session_id` is one of the close-type ops (SPEC-V0473-408):
                // its refusal is logged like a refused close (SPEC-V0473-108 AC1).
                append_session_event_with_detail(
                    handoff,
                    EVENT_CLOSE_REFUSED,
                    Some(&id),
                    ctx.agent_id.as_deref(),
                    Some(format!("op=pause; owner={owner}")),
                );
                anyhow::bail!(
                    "Session {id} is active and owned by another agent ({owner}); it was not \
                     paused. Pass force_other_owner:true with a reason to override."
                );
            }
            Resolution::Ambiguous { ids } => anyhow::bail!(ambiguous_message(&ids)),
            _ => {}
        }
    } else if pause_all {
        for entry in own_active_group(&entries, &caller) {
            if transition_group_member(ctx, &sessions_dir, entry, &caller, "paused", &mut notes)? {
                total_paused += 1;
            }
        }
    }

    if is_pause_only {
        let mut msg = String::new();
        if total_paused > 0 {
            msg.push_str(&format!("Paused {} session(s)", total_paused));
        }
        if let Some(id) = pause_id {
            if total_paused == 0 {
                msg.push_str(&format!(
                    "Warning: pause_session_id '{id}' not found among active/open sessions"
                ));
            }
        }
        if !pause_all && pause_id.is_none() {
            msg.push_str("Warning: pause_only=true requires pause_session_id or pause_active=true");
        }
        if msg.is_empty() {
            msg.push_str("No sessions were paused (none active)");
        }
        for note in &notes {
            msg.push('\n');
            msg.push_str(note);
        }
        return Ok(msg);
    }

    let session_status = arguments
        .get("session_status")
        .and_then(|v| v.as_str())
        .unwrap_or("closed");
    let keep_active = session_status == "active";

    let git_state = capture_git_state(project_dir)?;
    let now = Utc::now().to_rfc3339();

    let target_session_id = arguments.get("session_id").and_then(|v| v.as_str());

    // Scope is always auto-detected (spec §4.1 FR-2.1) — never accepted as
    // an argument — and only meaningful for a session actually becoming/
    // staying active; a session being closed doesn't need re-tagging.
    let detected_scope = crate::storage::detect_session_scope(project_dir).to_string();
    let (scope, agent_id, worktree, parent_session_id) = if keep_active {
        let parent = if detected_scope == "worktree" {
            find_active_primary_session_id(&sessions_dir, ctx.agent_id.as_deref())?
        } else {
            None
        };
        (
            Some(detected_scope.clone()),
            ctx.agent_id.clone(),
            Some(call_worktree.clone()),
            parent,
        )
    } else {
        (None, None, None, None)
    };

    let handoff_updates = SessionData {
        version: 2,
        id: None,
        ended_at: Some(now),
        summary: summary.to_string(),
        branch: Some(git_state.branch),
        commit: Some(git_state.commit),
        dirty_files: git_state.dirty_files,
        decisions: extract_array(arguments, "decisions"),
        blockers: extract_string_array(arguments, "blockers"),
        checklist: extract_array(arguments, "checklist"),
        handoff_notes: extract_array(arguments, "handoff_notes"),
        references: extract_array(arguments, "references"),
        context_pointers: extract_array(arguments, "context_pointers"),
        environment: arguments.get("environment").cloned(),
        timeline: arguments
            .get("timeline")
            .and_then(|v| v.as_str())
            .map(String::from),
        label: arguments
            .get("label")
            .and_then(|v| v.as_str())
            .map(String::from),
        parent_session_id,
        related_task_ids: extract_string_array(arguments, "related_task_ids"),
        owner_v: agent_id
            .as_ref()
            .map(|_| crate::storage::session_target::OWNER_V),
        agent_id,
        worktree: worktree.clone(),
        scope,
        // Lineage fields are stamped only on a session this call creates
        // (`lineage::stamp_new`); updates never carry them.
        ..Default::default()
    };

    // Write a brand-new session for this caller. A closed one records its
    // creator and place explicitly, because `handoff_updates` leaves ownership
    // unset for a closing call so that closing an *existing* session keeps the
    // owner it was created with. A new session also records which sessions it
    // continues (`lineage`); updating or closing an existing one never does.
    let explicit_continues: Option<Vec<String>> = arguments
        .get("continues")
        .and_then(|v| v.as_array())
        .map(|_| extract_string_array(arguments, "continues"));
    let lineage_token = arguments.get("lineage_token").and_then(|v| v.as_str());
    let create_new = || -> Result<(std::path::PathBuf, String, lineage::NewContinues)> {
        let new_id = generate_session_id();
        let mut data = handoff_updates.clone();
        data.id = Some(new_id.clone());
        let continues = lineage::continues_for_new(&lineage::NewSessionLineage {
            ctx,
            sessions_dir: &sessions_dir,
            entries: &entries,
            explicit: explicit_continues.clone(),
            token: lineage_token,
            parent: data.parent_session_id.as_deref(),
            scope: &detected_scope,
        })?;
        lineage::stamp_new(&mut data, continues.continues.clone(), project_dir);
        let target_status = if keep_active { "active" } else { "closed" };
        if !keep_active {
            data.agent_id = ctx.agent_id.clone();
            data.owner_v = ctx
                .agent_id
                .as_ref()
                .map(|_| crate::storage::session_target::OWNER_V);
            data.worktree = Some(call_worktree.clone());
            data.scope = Some(detected_scope.clone());
        }
        let p = write_session_with_status(&sessions_dir, &data, target_status)?;
        append_session_event(
            handoff,
            "session.created",
            Some(&new_id),
            ctx.agent_id.as_deref(),
        );
        Ok((p, new_id, continues))
    };

    let op = if close_id.is_some() || !keep_active {
        TargetOp::Close
    } else {
        TargetOp::Overwrite
    };
    let explicit_id = close_id.or(target_session_id);

    let mut created_continues: Option<Vec<String>> = None;
    let (total_closed, path, session_id) = if pause_id.is_some() || pause_all {
        (0, None, None)
    } else {
        match resolve_session_target(&entries, &caller, explicit_id, op, force) {
            Resolution::Target(t) => {
                let entry = entry_for(&entries, &t.id)
                    .ok_or_else(|| anyhow::anyhow!("Session '{}' disappeared", t.id))?;
                // The caller's decisions are applied whoever owns the session
                // (its summary too, except on a keep_active write into another
                // agent's session, see `WriteMode::MergeForeign` below), but
                // the branch/commit/dirty files describe the
                // CALLER's checkout: stamping them onto another agent's or
                // another worktree's session would mislead lineage inference.
                let foreign_checkout = t.foreign_owner.is_some()
                    || entry
                        .data
                        .worktree
                        .as_deref()
                        .is_some_and(|w| w != call_worktree);
                let mut updates = handoff_updates.clone();
                if foreign_checkout {
                    updates.branch = entry.data.branch.clone();
                    updates.commit = entry.data.commit.clone();
                    updates.dirty_files = entry.data.dirty_files.clone();
                }
                // The session was chosen from the read at the top of this call;
                // the write re-checks its owner under the sessions lock, so a
                // session adopted in the meantime is not closed or rewritten.
                let seen = SeenSession::of(entry);
                if op == TargetOp::Close {
                    let closed_path = update_session_at(
                        &sessions_dir,
                        &seen,
                        &updates,
                        Some(arguments),
                        Some("closed"),
                        WriteMode::Replace,
                    )
                    .map_err(|e| write_refusal(ctx, e, "close"))?;
                    record_ownership_event(ctx, &t, reason);
                    notes.extend(t.warnings.iter().cloned());
                    // A forced close carries its justification on the close
                    // event itself (SPEC-V0473-108 AC2).
                    let closed_detail = t.forced.then(|| {
                        serde_json::json!({
                            "reason": reason,
                            "forced": true,
                            "previous_owner": t.foreign_owner,
                        })
                        .to_string()
                    });
                    append_session_event_with_detail(
                        handoff,
                        "session.closed",
                        Some(&t.id),
                        ctx.agent_id.as_deref(),
                        closed_detail,
                    );
                    (1, Some(closed_path), Some(t.id))
                } else {
                    // Rewriting another agent's active session must not erase
                    // what its owner recorded: the caller's entries are added
                    // and the owner's summary stays.
                    let mode = if t.foreign_owner.is_some() {
                        WriteMode::MergeForeign
                    } else {
                        WriteMode::Replace
                    };
                    let updated_path = update_session_at(
                        &sessions_dir,
                        &seen,
                        &updates,
                        Some(arguments),
                        None,
                        mode,
                    )
                    .map_err(|e| write_refusal(ctx, e, "update"))?;
                    record_ownership_event(ctx, &t, reason);
                    notes.extend(t.warnings.iter().cloned());
                    if mode == WriteMode::MergeForeign {
                        notes.push(format!(
                            "Note: session {} belongs to another agent; your decisions, \
                             checklist, notes and pointers were appended and its summary was kept.",
                            t.id
                        ));
                    }
                    (0, Some(updated_path), Some(t.id))
                }
            }
            Resolution::NoTarget => {
                let (p, new_id, continues) = create_new()?;
                notes.extend(continues.warnings);
                if continues.uncertain {
                    notes.push("continues_uncertain: true".to_string());
                }
                created_continues = Some(continues.continues);
                (0, Some(p), Some(new_id))
            }
            Resolution::NotFound => match (close_id, target_session_id) {
                (Some(_), _) => (0, None, None),
                (None, Some(tid)) => {
                    anyhow::bail!("session_id '{tid}' not found among active sessions")
                }
                (None, None) => unreachable!("an implicit choice never reports NotFound"),
            },
            Resolution::Refused { id, owner } => {
                // Another agent's active session is never closed from outside.
                // Nothing is written; the refusal is only logged.
                append_session_event_with_detail(
                    handoff,
                    EVENT_CLOSE_REFUSED,
                    Some(&id),
                    ctx.agent_id.as_deref(),
                    Some(format!("op=close; owner={owner}")),
                );
                anyhow::bail!(
                    "Session {id} is active and owned by another agent ({owner}); it was not \
                     closed and nothing was saved. To close it anyway, pass force_other_owner:true \
                     with a reason."
                );
            }
            Resolution::Ambiguous { ids } => anyhow::bail!(ambiguous_message(&ids)),
            Resolution::NeedsExplicitId { ids } => anyhow::bail!(
                "An active session owned by another agent exists in this worktree ({}), and \
                 this caller has no agent identity (CLI). Pass session_id to say which session \
                 you mean.",
                ids.join(", ")
            ),
        }
    };

    let prune_lines = prune_sessions_per_config(handoff, ctx.agent_id.as_deref())?;

    let file_display = path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "(none)".to_string());
    let sid_display = session_id.as_deref().unwrap_or("(no active session)");
    let mut msg = format!(
        "Session saved: {}\nSession ID: {}\nFile: {}",
        summary, sid_display, file_display
    );

    if keep_active {
        msg.push_str("\nSession kept active (session_status: active)");
    }
    if let Some(continues) = created_continues.filter(|c| !c.is_empty()) {
        msg.push_str(&format!("\nContinues: {}", continues.join(", ")));
    }

    if total_paused > 0 {
        msg.push_str(&format!("\nPaused {} session(s)", total_paused));
    }
    if let Some(id) = pause_id {
        if total_paused == 0 {
            msg.push_str(&format!(
                "\nWarning: pause_session_id '{id}' not found among active/open sessions"
            ));
        }
    }
    if total_closed > 0 {
        msg.push_str(&format!("\nClosed {} previous session(s)", total_closed));
    }
    if let Some(id) = close_id {
        if total_closed == 0 {
            msg.push_str(&format!(
                "\nWarning: close_session_id '{id}' not found among active/open/paused sessions"
            ));
        }
    }
    for line in &prune_lines {
        msg.push('\n');
        msg.push_str(line);
    }
    for note in &notes {
        msg.push('\n');
        msg.push_str(note);
    }

    // FR-503: one metrics snapshot per day, refreshed on every non-pause-only
    // save. A snapshot failure must not fail the session save that already
    // succeeded, so it is surfaced as a warning in the response instead.
    if let Err(e) = super::metrics::write_snapshot(handoff) {
        msg.push_str(&format!("\nWarning: metrics snapshot failed: {e:#}"));
    }

    for w in collect_save_warnings(&handoff_updates, project_dir) {
        msg.push_str(&format!("\n{w}"));
    }

    Ok(msg)
}

/// Applies the opt-in `[settings] session_prune_limit` (v0.47.3): without it
/// nothing is ever deleted. Each removed session is logged as a
/// `session.pruned` event, and sessions kept above the limit because they are
/// heads are reported. Returns the human-readable lines for the tool response.
///
/// An unreadable `config.toml` is treated as "no limit configured": pruning
/// deletes user data, so the safe fallback is to delete nothing (the same
/// broken file already fails every config-reading tool loudly).
pub(super) fn prune_sessions_per_config(
    handoff: &Path,
    agent_id: Option<&str>,
) -> Result<Vec<String>> {
    let config_path = handoff.join("config.toml");
    let Some(limit) = read_config(&config_path)
        .ok()
        .and_then(|c| c.settings.session_prune_limit)
    else {
        return Ok(Vec::new());
    };

    let sessions_dir = handoff.join("sessions");
    let heads = closed_head_ids(&sessions_dir)?;
    let outcome = prune_closed_sessions(&sessions_dir, limit, &heads)?;
    for pruned in &outcome.removed {
        // Best-effort like every other event: the delete already happened.
        let _ = crate::storage::events::append_event(
            handoff,
            crate::storage::events::EventRecord {
                ts: Utc::now().to_rfc3339(),
                event: "session.pruned".to_string(),
                task_id: None,
                agent_id: agent_id.map(String::from),
                session_id: Some(pruned.id.clone()),
                detail: Some(format!(
                    "file={}; session_prune_limit={limit}",
                    pruned.file_name
                )),
            },
        );
    }

    let mut lines = Vec::new();
    if !outcome.removed.is_empty() {
        lines.push(format!(
            "Removed {} old session(s) (session_prune_limit: {limit})",
            outcome.removed.len()
        ));
    }
    if outcome.retained_for_head > 0 {
        lines.push(format!(
            "Warning: kept {} session(s) above session_prune_limit ({limit}) because they are heads",
            outcome.retained_for_head
        ));
    }
    Ok(lines)
}

/// Best-effort append of a session lifecycle event (`session.created` /
/// `activated` / `resumed` / `closed`, spec 3.6.3, 4.2 FR-2.5) to
/// `events.jsonl`. A logging failure never fails the tool call — same
/// fire-and-forget contract as the other event sites (e.g.
/// `claim_release::handle_reclaim`).
pub(super) fn append_session_event(
    handoff: &Path,
    event: &str,
    session_id: Option<&str>,
    agent_id: Option<&str>,
) {
    append_session_event_with_detail(handoff, event, session_id, agent_id, None);
}

/// [`append_session_event`] with a free-form `detail` (ownership events carry
/// the owner, the override flag and the caller's reason).
pub(super) fn append_session_event_with_detail(
    handoff: &Path,
    event: &str,
    session_id: Option<&str>,
    agent_id: Option<&str>,
    detail: Option<String>,
) {
    let _ = crate::storage::events::append_event(
        handoff,
        crate::storage::events::EventRecord {
            ts: Utc::now().to_rfc3339(),
            event: event.to_string(),
            task_id: None,
            agent_id: agent_id.map(String::from),
            session_id: session_id.map(String::from),
            detail,
        },
    );
}

/// The entry for `id` (an exact id, as [`Target::id`] carries).
pub(super) fn entry_for<'a>(entries: &'a [SessionEntry], id: &str) -> Option<&'a SessionEntry> {
    entries.iter().find(|e| e.data.id.as_deref() == Some(id))
}

/// Log the ownership event a resolved [`Target`] calls for (none when the
/// caller owns the session). `reason` is `force_other_owner`'s justification.
pub(super) fn record_ownership_event(ctx: &HandlerContext, target: &Target, reason: Option<&str>) {
    let Some(event) = target.event else { return };
    let mut detail = format!(
        "owner={}",
        target.foreign_owner.as_deref().unwrap_or("none")
    );
    if target.forced {
        detail.push_str("; forced=true");
    }
    if let Some(r) = reason {
        detail.push_str(&format!("; reason={r}"));
    }
    append_session_event_with_detail(
        &ctx.handoff_dir,
        event,
        Some(&target.id),
        ctx.agent_id.as_deref(),
        Some(detail),
    );
}

/// The error for a checked session write that did not happen because the
/// session changed since this call read it (`op` is `close`, `pause` or
/// `update`). A change of owner is logged like any other refused close; nothing
/// was written either way. Errors that are not about the session changing pass
/// through.
pub(super) fn write_refusal(ctx: &HandlerContext, err: anyhow::Error, op: &str) -> anyhow::Error {
    let past = match op {
        "close" => "closed",
        "pause" => "paused",
        _ => "updated",
    };
    match err.downcast_ref::<SessionWriteError>() {
        Some(SessionWriteError::OwnerChanged { id, owner }) => {
            let owner = owner.as_deref().unwrap_or("none");
            append_session_event_with_detail(
                &ctx.handoff_dir,
                EVENT_CLOSE_REFUSED,
                Some(id),
                ctx.agent_id.as_deref(),
                Some(format!("op={op}; owner={owner}; changed_during_call=true")),
            );
            anyhow::anyhow!(
                "Session {id} changed owner while this call was running (now owned by {owner}); \
                 it was not {past} and nothing was saved."
            )
        }
        Some(SessionWriteError::Gone { id }) => anyhow::anyhow!(
            "Session {id} was closed or removed by another process while this call was \
             running; it was not {past} and nothing was saved."
        ),
        None => err,
    }
}

/// Move one session of a bulk group (`pause_active`, an import's close) to
/// `to`. Returns whether it moved. A session that changed hands since the group
/// was read is left alone and reported in `notes`; an ownerless one is moved
/// with the same warning and `session.foreign_close` event a single-session
/// close would produce.
pub(super) fn transition_group_member(
    ctx: &HandlerContext,
    sessions_dir: &Path,
    entry: &SessionEntry,
    caller: &Caller<'_>,
    to: &str,
    notes: &mut Vec<String>,
) -> Result<bool> {
    let id = entry.data.id.as_deref().unwrap_or_default();
    match transition_session_checked(sessions_dir, &SeenSession::of(entry), to) {
        Ok(_) => {
            if let Some(warning) = ownerless_bulk_warning(entry, caller) {
                append_session_event_with_detail(
                    &ctx.handoff_dir,
                    EVENT_FOREIGN_CLOSE,
                    Some(id),
                    ctx.agent_id.as_deref(),
                    Some("owner=none".to_string()),
                );
                notes.push(warning);
            }
            Ok(true)
        }
        Err(e) if e.downcast_ref::<SessionWriteError>().is_some() => {
            notes.push(format!(
                "Warning: session {id} was changed by another process ({e}); it was left as it is."
            ));
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// Error text for a close that matches several of the caller's own sessions.
pub(super) fn ambiguous_message(ids: &[String]) -> String {
    format!(
        "{} sessions match ({}). Pass session_id / close_session_id to say which one you mean.",
        ids.len(),
        ids.join(", ")
    )
}

fn collect_save_warnings(data: &SessionData, project_dir: &Path) -> Vec<String> {
    let mut warnings = Vec::new();

    if data.checklist.is_empty() {
        warnings.push(
            "Warning: No checklist items. Consider adding verification items for the next session."
                .to_string(),
        );
    } else {
        let unchecked: Vec<&str> = data
            .checklist
            .iter()
            .filter_map(|item| {
                let checked = item
                    .get("checked")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !checked {
                    item.get("item").and_then(|v| v.as_str())
                } else {
                    None
                }
            })
            .collect();

        if !unchecked.is_empty() {
            warnings.push(format!(
                "Warning: {} unchecked checklist item(s) \u{2014} {}",
                unchecked.len(),
                unchecked.join(", ")
            ));
        }
    }

    let has_suggestion = data.handoff_notes.iter().any(|note| {
        note.get("category")
            .and_then(|v| v.as_str())
            .is_some_and(|c| c == "suggestion")
    });
    if !has_suggestion {
        warnings.push(
            "Warning: No 'suggestion' handoff_notes \u{2014} the next session won't know what to \
             do first. Add at least one note with category 'suggestion' describing the recommended \
             next action."
                .to_string(),
        );
    }

    if data.context_pointers.is_empty() {
        warnings.push(
            "Warning: No context_pointers. The next session won't know which files to read first."
                .to_string(),
        );
    }

    if data.decisions.is_empty() {
        warnings.push(
            "Warning: No decisions recorded. Consider documenting key decisions made during this session."
                .to_string(),
        );
    }

    if data.references.is_empty() {
        warnings.push(
            "Warning: No references. Consider adding links to relevant docs, issues, or MRs."
                .to_string(),
        );
    } else {
        for r in &data.references {
            let uri = r.get("uri").and_then(|v| v.as_str()).unwrap_or("");
            if uri.is_empty() {
                continue;
            }
            if uri.starts_with("http://") || uri.starts_with("https://") {
                continue;
            }
            if uri.starts_with("ref-") {
                continue;
            }
            let resolved = if Path::new(uri).is_absolute() {
                std::path::PathBuf::from(uri)
            } else {
                project_dir.join(uri)
            };
            let check_path = resolved
                .to_string_lossy()
                .split('#')
                .next()
                .unwrap_or("")
                .to_string();
            if !Path::new(&check_path).exists() {
                let label = r.get("label").and_then(|v| v.as_str()).unwrap_or("");
                warnings.push(format!(
                    "Warning: reference '{label}' path does not exist: {uri}"
                ));
            }
        }
    }

    for cp in &data.context_pointers {
        let p = cp.get("path").and_then(|v| v.as_str()).unwrap_or("");
        if p.is_empty() {
            continue;
        }
        let resolved = if Path::new(p).is_absolute() {
            std::path::PathBuf::from(p)
        } else {
            project_dir.join(p)
        };
        if !resolved.exists() {
            warnings.push(format!("Warning: context_pointer path does not exist: {p}"));
        }
    }

    warnings
}

fn extract_array(val: &Value, key: &str) -> Vec<Value> {
    val.get(key)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

fn extract_string_array(val: &Value, key: &str) -> Vec<String> {
    val.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}
