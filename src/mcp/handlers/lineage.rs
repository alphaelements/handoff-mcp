//! The handler side of session lineage (FR-V0473-009): which closed session a
//! `load_context` call reports as `previous_session`, and which sessions a
//! newly created session records in `continues`.
//!
//! The rules themselves are pure and live in [`crate::storage::lineage`]; this
//! module gathers their inputs (closed sessions, git, the caller's identity)
//! and keeps the one piece of process state they need: the previous session
//! `load_context` settled on, held until the next session creation uses it.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};

use super::HandlerContext;
use crate::storage::git::{
    capture_git_state, current_branch, head_history, recent_commit_subjects, repo_root,
};
use crate::storage::lineage::{
    decide, dirty_paths_from_porcelain, extract_task_ids, find_heads, Decision, GitFacts, Identity,
    Ranked, HISTORY_DEPTH, LINEAGE_V,
};
use crate::storage::session_target::Caller;
use crate::storage::sessions::{
    find_active_primary_session_id, ids_match, read_recent_closed_sessions, read_session_by_id,
    read_sessions_by_status, SessionData, SessionEntry, RECENT_CLOSED_MAX,
};

/// Commit subjects scanned for task ids.
const SUBJECT_DEPTH: usize = 5;

/// Where a retained value lives: one `.handoff` and one checkout of it. Two
/// worktrees sharing a `.handoff` never use each other's value.
type Key = (PathBuf, String);

#[derive(Debug, Clone)]
struct Retained {
    token: String,
    session_id: String,
}

/// The previous session each `load_context` settled on, waiting for the next
/// session creation (SPEC-V0473-509). Process memory only: a restarted server
/// has none and the creation decides for itself.
static RETAINED: LazyLock<Mutex<HashMap<Key, Retained>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static TOKEN_SEQ: AtomicU64 = AtomicU64::new(0);

fn retained_map() -> std::sync::MutexGuard<'static, HashMap<Key, Retained>> {
    // A poisoned lock only means another thread panicked mid-update of a map
    // of plain strings; the data is still usable.
    RETAINED.lock().unwrap_or_else(|e| e.into_inner())
}

fn key_of(handoff_dir: &Path, worktree: &str) -> Key {
    (handoff_dir.to_path_buf(), worktree.to_string())
}

/// Remember `session_id` as the session the next creation here continues and
/// return the `lineage_token` that names it.
pub fn retain(handoff_dir: &Path, worktree: &str, session_id: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let token = format!(
        "lt-{nanos:x}-{:x}",
        TOKEN_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    retained_map().insert(
        key_of(handoff_dir, worktree),
        Retained {
            token: token.clone(),
            session_id: session_id.to_string(),
        },
    );
    token
}

/// Drop whatever is retained for this checkout.
pub fn forget(handoff_dir: &Path, worktree: &str) {
    retained_map().remove(&key_of(handoff_dir, worktree));
}

/// Use up a retained value: the one `token` names (from any checkout), else
/// this checkout's. An unknown or spent token yields a warning and falls back
/// to the checkout's value.
fn take_retained(
    handoff_dir: &Path,
    worktree: &str,
    token: Option<&str>,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let mut map = retained_map();
    if let Some(token) = token {
        let found = map
            .iter()
            .find(|((dir, _), r)| dir == handoff_dir && r.token == token)
            .map(|(k, _)| k.clone());
        match found {
            Some(k) => return map.remove(&k).map(|r| r.session_id),
            None => warnings.push(format!(
                "Warning: lineage_token '{token}' is unknown or already used; ignored."
            )),
        }
    }
    map.remove(&key_of(handoff_dir, worktree))
        .map(|r| r.session_id)
}

/// What `load_context` reports about the previous session.
#[derive(Debug)]
pub enum Previous {
    /// No closed session is a head (a fresh project).
    None,
    /// Named by `continue_from`.
    Explicit(SessionData),
    /// The caller's own active session already continues it.
    FromActive(SessionData),
    Confirmed {
        session: SessionData,
        evidence: Value,
    },
    Ambiguous {
        /// At most five, best first; bodies are never included.
        other_heads: Vec<Value>,
        stale: bool,
        /// Some candidate is continued by another agent's active session.
        in_flight: bool,
        /// One of those active sessions lives in the caller's own checkout, so
        /// it may be the caller's own (after a restart or reconnect) and
        /// `adopt` is a sensible suggestion.
        in_flight_here: bool,
        /// The highest-ranked candidate, for a creation that must record one.
        best: String,
    },
}

/// Inputs to [`resolve_previous`].
pub struct PreviousRequest<'a> {
    pub ctx: &'a HandlerContext,
    pub sessions_dir: &'a Path,
    pub entries: &'a [SessionEntry],
    /// The caller's own active session, if it has one.
    pub own_active: Option<&'a SessionData>,
    pub continue_from: Option<&'a str>,
    pub claimed: &'a [String],
}

/// Decide what the caller continues. Order: `continue_from`, then the caller's
/// own active session's `continues`, then the head selection.
pub fn resolve_previous(req: &PreviousRequest<'_>, warnings: &mut Vec<String>) -> Result<Previous> {
    let closed = read_recent_closed_sessions(req.sessions_dir, RECENT_CLOSED_MAX)?;

    if let Some(wanted) = req.continue_from {
        match find_closed(&closed, req.sessions_dir, wanted)? {
            Some(session) => return Ok(Previous::Explicit(session)),
            None => warnings.push(format!(
                "Warning: continue_from '{wanted}' is not a closed session; choosing the \
                 previous session automatically."
            )),
        }
    }

    if let Some(first) = req.own_active.and_then(|a| a.continues_ids().first()) {
        if let Some(session) = find_closed(&closed, req.sessions_dir, first)? {
            return Ok(Previous::FromActive(session));
        }
    }

    // A worktree session is created under the primary's active session and
    // continues it, so what it reports is that session's own previous session:
    // no scoring, no git, and no in-flight block for the orchestrator's claim.
    if let Some(session) = primary_parent_previous(req, &closed)? {
        return Ok(Previous::FromActive(session));
    }

    // The caller's own active session is not somebody else's claim on a head:
    // reaching this point with one means it could not name its previous session.
    let own_id = req.own_active.and_then(|a| a.id.as_deref());
    let actives: Vec<&SessionData> = req
        .entries
        .iter()
        .filter(|e| e.status == "active" && (own_id.is_none() || e.data.id.as_deref() != own_id))
        .map(|e| &e.data)
        .collect();
    let heads = find_heads(&closed, &actives);
    let identity = Identity {
        agent_id: req.ctx.agent_id.clone(),
        worktree_key: crate::storage::worktree_key(&req.ctx.project_dir),
        claimed: req.claimed.iter().cloned().collect::<BTreeSet<_>>(),
    };
    let project_dir = &req.ctx.project_dir;
    let worktree = project_dir.to_string_lossy();
    let caller = Caller {
        agent_id: req.ctx.agent_id.as_deref(),
        worktree: &worktree,
    };
    Ok(
        match decide(&heads, Utc::now(), &identity, || {
            gather_git_facts(project_dir)
        }) {
            Decision::NoHeads => Previous::None,
            Decision::Confirmed { head, evidence } => Previous::Confirmed {
                session: head.session.clone(),
                evidence,
            },
            Decision::Ambiguous { ranked, stale } => ambiguous(&ranked, stale, &actives, &caller),
        },
    )
}

/// The first closed session the primary's active session continues, when the
/// caller works in a linked worktree and such a session exists.
fn primary_parent_previous(
    req: &PreviousRequest<'_>,
    closed: &[SessionData],
) -> Result<Option<SessionData>> {
    if crate::storage::detect_session_scope(&req.ctx.project_dir) != "worktree" {
        return Ok(None);
    }
    let Some(parent_id) = find_active_primary_session_id(req.sessions_dir)? else {
        return Ok(None);
    };
    let first = req
        .entries
        .iter()
        .find(|e| e.status == "active" && e.data.id.as_deref() == Some(parent_id.as_str()))
        .and_then(|e| e.data.continues_ids().first());
    match first {
        Some(first) => find_closed(closed, req.sessions_dir, first),
        None => Ok(None),
    }
}

fn ambiguous(
    ranked: &[Ranked<'_>],
    stale: bool,
    actives: &[&SessionData],
    caller: &Caller<'_>,
) -> Previous {
    let in_flight = ranked.iter().any(|r| !r.head.in_flight_by.is_empty());
    let in_flight_here = ranked
        .iter()
        .flat_map(|r| r.head.in_flight_by.iter())
        .filter_map(|id| {
            actives
                .iter()
                .find(|a| a.id.as_deref() == Some(id.as_str()))
        })
        .any(|a| caller.in_worktree(a));
    let best = ranked
        .first()
        .and_then(|r| r.head.session.id.clone())
        .unwrap_or_default();
    Previous::Ambiguous {
        other_heads: ranked.iter().map(|r| other_head_json(r, stale)).collect(),
        stale,
        in_flight,
        in_flight_here,
        best,
    }
}

/// One entry of `other_heads`: enough to recognise the lineage and to pass its
/// id as `continue_from`, never the session body.
fn other_head_json(r: &Ranked<'_>, stale: bool) -> Value {
    let s = r.head.session;
    let mut obj = json!({
        "id": s.id,
        "summary": s.summary,
        "branch": s.branch,
        "related_task_ids": s.related_task_ids,
        "ended_at": s.ended_at,
    });
    if let Some(label) = &s.label {
        obj["label"] = json!(label);
    }
    if !r.head.in_flight_by.is_empty() {
        obj["in_flight_by"] = json!(r.head.in_flight_by);
    }
    if stale {
        obj["stale"] = json!(true);
    }
    obj
}

/// A closed session by id, exact or unambiguous prefix. The recent list is
/// searched first; an older one is read from disk.
fn find_closed(
    recent: &[SessionData],
    sessions_dir: &Path,
    wanted: &str,
) -> Result<Option<SessionData>> {
    let pick = |list: &[SessionData]| -> Option<SessionData> {
        let exact = list.iter().find(|s| s.id.as_deref() == Some(wanted));
        if exact.is_some() {
            return exact.cloned();
        }
        let mut prefixed = list
            .iter()
            .filter(|s| s.id.as_deref().is_some_and(|id| ids_match(id, wanted)));
        match (prefixed.next(), prefixed.next()) {
            (Some(only), None) => Some(only.clone()),
            _ => None,
        }
    };
    if let Some(found) = pick(recent) {
        return Ok(Some(found));
    }
    Ok(pick(&read_sessions_by_status(sessions_dir, "closed")?))
}

/// Git facts for scoring. Anything git cannot answer stays empty and scores 0.
fn gather_git_facts(project_dir: &Path) -> GitFacts {
    let branch = current_branch(project_dir);
    let mut task_ids = BTreeSet::new();
    if let Some(b) = &branch {
        task_ids.extend(extract_task_ids(b));
    }
    for subject in recent_commit_subjects(project_dir, SUBJECT_DEPTH) {
        task_ids.extend(extract_task_ids(&subject));
    }
    // `capture_git_state` degrades to placeholders itself and never fails.
    let dirty_paths = capture_git_state(project_dir)
        .map(|g| dirty_paths_from_porcelain(&g.dirty_files))
        .unwrap_or_default();
    GitFacts {
        branch,
        history: head_history(project_dir, HISTORY_DEPTH),
        task_ids,
        dirty_paths,
        root: repo_root(project_dir),
    }
}

/// Arguments of [`continues_for_new`].
pub struct NewSessionLineage<'a> {
    pub ctx: &'a HandlerContext,
    pub sessions_dir: &'a Path,
    pub entries: &'a [SessionEntry],
    /// `save_context(continues)`: wins over everything.
    pub explicit: Option<Vec<String>>,
    pub token: Option<&'a str>,
    /// The primary session a worktree session hangs off, if any.
    pub parent: Option<&'a str>,
    /// Auto-detected scope of the checkout (`primary` / `worktree` / ...).
    pub scope: &'a str,
}

/// What a new session records.
#[derive(Debug, Default)]
pub struct NewContinues {
    pub continues: Vec<String>,
    pub warnings: Vec<String>,
    /// The server could not tell and recorded its best guess.
    pub uncertain: bool,
}

/// The `continues` of a session about to be created (SPEC-V0473-509, 609):
/// explicit `continues` > a worktree session's parent > `lineage_token` > this
/// checkout's retained value > the server's own decision.
pub fn continues_for_new(req: &NewSessionLineage<'_>) -> Result<NewContinues> {
    let handoff = &req.ctx.handoff_dir;
    let worktree = req.ctx.project_dir.to_string_lossy().to_string();
    let mut out = NewContinues::default();

    if let Some(explicit) = &req.explicit {
        forget(handoff, &worktree);
        for id in explicit {
            match read_session_by_id(req.sessions_dir, id) {
                Ok(Some(found)) => out
                    .continues
                    .push(found.id.unwrap_or_else(|| id.to_string())),
                Ok(None) | Err(_) => {
                    // Kept as given: the id may belong to a session another
                    // machine has not synced yet, and dropping it would lose
                    // the caller's explicit statement.
                    out.warnings.push(format!(
                        "Warning: continues '{id}' does not match a known session; recorded as given."
                    ));
                    out.continues.push(id.clone());
                }
            }
        }
        return Ok(out);
    }

    if let (Some(parent), "worktree") = (req.parent, req.scope) {
        out.continues.push(parent.to_string());
        return Ok(out);
    }

    if let Some(id) = take_retained(handoff, &worktree, req.token, &mut out.warnings) {
        out.continues.push(id);
        return Ok(out);
    }

    let claimed = claimed_tasks(req.ctx);
    let mut warnings = Vec::new();
    let previous = resolve_previous(
        &PreviousRequest {
            ctx: req.ctx,
            sessions_dir: req.sessions_dir,
            entries: req.entries,
            own_active: None,
            continue_from: None,
            claimed: &claimed,
        },
        &mut warnings,
    )?;
    out.warnings.extend(warnings);
    match previous {
        Previous::Confirmed { session, .. } | Previous::Explicit(session) => {
            out.continues.extend(session.id);
        }
        Previous::FromActive(session) => out.continues.extend(session.id),
        Previous::None => {}
        Previous::Ambiguous { best, stale, .. } => {
            out.continues.push(best);
            out.uncertain = true;
            out.warnings.push(format!(
                "Warning: continues_uncertain: the previous session could not be told apart{}; \
                 recorded the best candidate. Pass continues (or continue_from to \
                 handoff_load_context) to be explicit.",
                if stale {
                    " (every candidate is old)"
                } else {
                    ""
                }
            ));
        }
    }
    Ok(out)
}

/// Tasks the calling agent has claimed. A caller with no agent record (the CLI,
/// or an agent that never called `load_context`) has claimed nothing, which is
/// what the empty set says. An unreadable record is treated the same way: the
/// claims only add task-overlap points to a score that is advisory, and the
/// record itself is reported by the tools that own it.
pub fn claimed_tasks(ctx: &HandlerContext) -> Vec<String> {
    ctx.agent_id
        .as_deref()
        .and_then(|id| {
            crate::storage::agents::read_agent(&ctx.handoff_dir, id)
                .ok()
                .flatten()
        })
        .map(|a| a.claimed_tasks)
        .unwrap_or_default()
}

/// The lineage fields every newly created session carries.
pub fn stamp_new(data: &mut SessionData, continues: Vec<String>, project_dir: &Path) {
    data.lineage_v = Some(LINEAGE_V);
    data.continues = Some(continues);
    data.worktree_key = crate::storage::worktree_key(project_dir);
}
