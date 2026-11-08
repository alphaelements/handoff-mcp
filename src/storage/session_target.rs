//! The one place that decides which session an operation touches
//! (FR-V0473-008, spec §4.4 / §4.4').
//!
//! Every path that picks a session — `save_context` (implicit close,
//! `keep_active` update, `pause_*`, `close_session_id`, `session_id`),
//! `update_session`, `import_context`, `auto_schedule`'s decision append,
//! `activate_open_sessions`, `load_context`'s selected session, `fork` and
//! `merge` — goes through [`resolve_session_target`] (single session) or
//! [`own_active_group`] (the "all of mine" group used by `pause_active` and
//! `import_context`). The handlers only act on what these return, so the
//! ownership rules live here and nowhere else.
//!
//! Vocabulary:
//! - *Mine*: the session's recorded owner is the caller's `agent_id`.
//! - *Unowned*: no trustworthy owner. Sessions written before `owner_v` carry
//!   an `agent_id` from the old per-call generator, so they count as unowned
//!   (spec §4.4' B1), as do sessions written by an unidentified caller (CLI).
//! - *Foreign*: owned by a different agent.
//!
//! "Mine" for an *implicit* choice additionally requires the caller's
//! worktree (a session with no recorded worktree matches any), so a
//! sub-agent saving from each worktree of a multi-worktree run keeps its own
//! session per worktree.

use super::sessions::{ids_match, SessionData, SessionEntry};

/// Current ownership format (`SessionData::owner_v`).
pub const OWNER_V: u32 = 1;

/// Event for an operation that closes/pauses/merges away a session owned by
/// someone else, or by nobody.
pub const EVENT_FOREIGN_CLOSE: &str = "session.foreign_close";
/// Event for an operation that writes into a session owned by someone else.
pub const EVENT_FOREIGN_WRITE: &str = "session.foreign_write";
/// Event for a close/pause/merge that was refused because the target is
/// another agent's active session.
pub const EVENT_CLOSE_REFUSED: &str = "session.close_refused";
/// Event for taking over a session (`load_context` with `adopt`).
pub const EVENT_ADOPTED: &str = "session.adopted";

/// Who is calling and from where.
#[derive(Debug, Clone, Copy)]
pub struct Caller<'a> {
    /// `None` for the CLI and for in-process callers that never registered.
    pub agent_id: Option<&'a str>,
    /// The caller's resolved `project_dir`.
    pub worktree: &'a str,
}

/// The owner to trust: `agent_id` only when `owner_v` vouches for it.
pub fn effective_owner(s: &SessionData) -> Option<&str> {
    s.owner_v.and(s.agent_id.as_deref())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    Mine,
    Unowned,
    Foreign,
}

impl Caller<'_> {
    pub fn relation(&self, s: &SessionData) -> Relation {
        match effective_owner(s) {
            None => Relation::Unowned,
            Some(owner) if Some(owner) == self.agent_id => Relation::Mine,
            Some(_) => Relation::Foreign,
        }
    }

    /// A session with no recorded worktree matches every caller (it predates
    /// the field).
    pub fn in_worktree(&self, s: &SessionData) -> bool {
        s.worktree.as_deref().is_none_or(|w| w == self.worktree)
    }
}

/// What the operation intends to do to the session. The same ownership rule
/// applies to every op, but a *foreign active* session is refused for the
/// ones that end it (close, pause, merge) and merely noted for the ones that
/// keep it going (overwrite, append, select).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetOp {
    /// Close it (`close_session_id`, `session_id` + closed, implicit close).
    Close,
    /// Pause it (`pause_session_id`).
    Pause,
    /// Rewrite its content (`save_context(session_id, keep_active)`).
    Overwrite,
    /// Add to it without removing anything (`update_session`, `auto_schedule`).
    Append,
    /// Load it (`load_context`). Taking it over is the handler's `adopt` rule.
    Select,
    /// Merge into it (`merge_sessions` target).
    Merge,
}

impl TargetOp {
    fn statuses(self) -> &'static [&'static str] {
        match self {
            TargetOp::Close | TargetOp::Select | TargetOp::Merge => &["active", "open", "paused"],
            TargetOp::Pause => &["active", "open"],
            TargetOp::Overwrite | TargetOp::Append => &["active"],
        }
    }

    fn refuses_foreign_active(self) -> bool {
        matches!(self, TargetOp::Close | TargetOp::Pause | TargetOp::Merge)
    }

    fn foreign_event(self) -> &'static str {
        match self {
            TargetOp::Close | TargetOp::Pause => EVENT_FOREIGN_CLOSE,
            TargetOp::Overwrite | TargetOp::Append | TargetOp::Select | TargetOp::Merge => {
                EVENT_FOREIGN_WRITE
            }
        }
    }
}

/// A session the operation may act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub id: String,
    pub status: String,
    /// The recorded owner when it is someone other than the caller.
    pub foreign_owner: Option<String>,
    /// Event to log because the caller is not the (known) owner.
    pub event: Option<&'static str>,
    /// The caller overrode the protection with `force_other_owner`.
    pub forced: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Target(Target),
    /// No session qualifies. The caller creates a new one (or errors, for the
    /// ops that cannot).
    NoTarget,
    /// An explicit id matched nothing in the statuses the op can act on.
    NotFound,
    /// An explicit id named another agent's active session and the op is
    /// destructive. Nothing was touched.
    Refused {
        id: String,
        owner: String,
    },
    /// Several of the caller's own sessions qualify and the op is too
    /// destructive to guess.
    Ambiguous {
        ids: Vec<String>,
    },
    /// The caller has no identity and a session owned by someone is active in
    /// this worktree, so it must say which one it means.
    NeedsExplicitId {
        ids: Vec<String>,
    },
}

fn entry_id(e: &SessionEntry) -> String {
    e.data.id.clone().unwrap_or_default()
}

/// Sort key for "most recently updated": the last save stamps `ended_at`,
/// and the id (which embeds the creation time) breaks ties and orders
/// sessions that never recorded one.
fn updated_key(e: &SessionEntry) -> (String, String) {
    (e.data.ended_at.clone().unwrap_or_default(), entry_id(e))
}

fn latest<'a>(list: &[&'a SessionEntry]) -> Option<&'a SessionEntry> {
    list.iter().copied().max_by_key(|e| updated_key(e))
}

/// Pick the session an operation acts on.
///
/// `explicit_id` is a caller-supplied id (exact or prefix); `None` means an
/// implicit choice, which only ever lands on the caller's own sessions (or
/// ownerless ones). `force` is `force_other_owner`.
pub fn resolve_session_target(
    entries: &[SessionEntry],
    caller: &Caller<'_>,
    explicit_id: Option<&str>,
    op: TargetOp,
    force: bool,
) -> Resolution {
    match explicit_id {
        Some(id) => resolve_explicit(entries, caller, id, op, force),
        None => resolve_implicit(entries, caller, op),
    }
}

fn resolve_explicit(
    entries: &[SessionEntry],
    caller: &Caller<'_>,
    id: &str,
    op: TargetOp,
    force: bool,
) -> Resolution {
    let matches: Vec<&SessionEntry> = entries
        .iter()
        .filter(|e| op.statuses().contains(&e.status.as_str()) && ids_match(&entry_id(e), id))
        .collect();
    match matches.as_slice() {
        [] => Resolution::NotFound,
        [e] => classify(e, caller, op, force),
        many => Resolution::Ambiguous {
            ids: many.iter().map(|e| entry_id(e)).collect(),
        },
    }
}

/// The warning for modifying a session that has no recorded owner.
fn ownerless_warning(id: &str) -> String {
    format!(
        "Warning: session {id} has no recorded owner (created before ownership tracking \
         or by the CLI); it was modified without an ownership check."
    )
}

/// What a bulk operation over [`own_active_group`] (`pause_active`, the
/// import's close) owes the user for taking `e`: the same warning
/// [`resolve_session_target`] gives for an ownerless session, and the caller
/// logs [`EVENT_FOREIGN_CLOSE`] with it. `None` when the session is the
/// caller's own, or when neither side has an identity (CLI), where nothing is
/// reported either.
pub fn ownerless_bulk_warning(e: &SessionEntry, caller: &Caller<'_>) -> Option<String> {
    (caller.agent_id.is_some() && caller.relation(&e.data) == Relation::Unowned)
        .then(|| ownerless_warning(&entry_id(e)))
}

/// Build the [`Target`] (or refusal) for one concrete session.
fn classify(e: &SessionEntry, caller: &Caller<'_>, op: TargetOp, force: bool) -> Resolution {
    let id = entry_id(e);
    let owner = effective_owner(&e.data).map(String::from);
    let mut target = Target {
        id: id.clone(),
        status: e.status.clone(),
        foreign_owner: None,
        event: None,
        forced: false,
        warnings: Vec::new(),
    };
    match (caller.relation(&e.data), caller.agent_id) {
        (Relation::Mine, _) => {}
        // Both sides unidentified (CLI session, CLI caller): the pre-ownership
        // behaviour, nothing to report.
        (Relation::Unowned, None) => {}
        // Loading an ownerless session changes nothing; the next write claims it.
        (Relation::Unowned, Some(_)) if op == TargetOp::Select => {}
        (Relation::Unowned, Some(_)) => {
            target.event = Some(op.foreign_event());
            target.warnings.push(ownerless_warning(&id));
        }
        (Relation::Foreign, None) => {
            target.foreign_owner = owner;
            if op != TargetOp::Select {
                target.event = Some(op.foreign_event());
                target.warnings.push(format!(
                    "Warning: session {id} is owned by another agent and this caller has no \
                     agent identity (CLI); it was modified anyway."
                ));
            }
        }
        (Relation::Foreign, Some(_)) => {
            let owner = owner.unwrap_or_default();
            target.foreign_owner = Some(owner.clone());
            let active = e.status == "active";
            if op == TargetOp::Select {
                // Taking over is the handler's `adopt` rule.
            } else if active && op.refuses_foreign_active() && !force {
                return Resolution::Refused { id, owner };
            } else if active && op.refuses_foreign_active() {
                target.forced = true;
                target.event = Some(op.foreign_event());
                target.warnings.push(format!(
                    "Warning: session {id} is owned by another agent ({owner}); proceeding \
                     because force_other_owner was set."
                ));
            } else {
                target.event = Some(op.foreign_event());
                target.warnings.push(format!(
                    "Warning: session {id} is owned by another agent ({owner}); it is {} and \
                     was modified anyway.",
                    if active { "active" } else { &e.status }
                ));
            }
        }
    }
    Resolution::Target(target)
}

fn resolve_implicit(entries: &[SessionEntry], caller: &Caller<'_>, op: TargetOp) -> Resolution {
    let active: Vec<&SessionEntry> = entries
        .iter()
        .filter(|e| e.status == "active" && caller.in_worktree(&e.data))
        .collect();
    let by = |rel: Relation| -> Vec<&SessionEntry> {
        active
            .iter()
            .copied()
            .filter(|e| caller.relation(&e.data) == rel)
            .collect()
    };
    let mine = by(Relation::Mine);
    let unowned = by(Relation::Unowned);

    if caller.agent_id.is_none() {
        // No identity: only ownerless sessions may be chosen implicitly, and
        // an owned one in this worktree means "say which".
        let foreign = by(Relation::Foreign);
        if !foreign.is_empty() {
            let mut ids: Vec<String> = foreign.iter().map(|e| entry_id(e)).collect();
            ids.extend(unowned.iter().map(|e| entry_id(e)));
            return Resolution::NeedsExplicitId { ids };
        }
        return pick_unowned(&unowned, caller, op);
    }

    match mine.as_slice() {
        [] => pick_unowned(&unowned, caller, op),
        [only] => classify(only, caller, op, false),
        many => {
            let ids: Vec<String> = many.iter().map(|e| entry_id(e)).collect();
            if op == TargetOp::Close {
                return Resolution::Ambiguous { ids };
            }
            let chosen = latest(many).expect("non-empty");
            let mut res = classify(chosen, caller, op, false);
            if let Resolution::Target(t) = &mut res {
                t.warnings.push(format!(
                    "Warning: {} of your sessions are active in this worktree ({}); used the \
                     most recently updated one ({}). Pass session_id to choose.",
                    ids.len(),
                    ids.join(", "),
                    t.id
                ));
            }
            res
        }
    }
}

/// Ownerless sessions are legacy data: as before ownership existed, the most
/// recent one is the target.
fn pick_unowned(unowned: &[&SessionEntry], caller: &Caller<'_>, op: TargetOp) -> Resolution {
    match latest(unowned) {
        None => Resolution::NoTarget,
        Some(e) => classify(e, caller, op, false),
    }
}

/// Every active session in the caller's worktree that the caller may act on
/// as "its own" at once: its own plus the ownerless ones (a caller with no
/// identity gets the ownerless ones only, and nothing at all when an owned
/// session is active here — it must name one).
pub fn own_active_group<'a>(
    entries: &'a [SessionEntry],
    caller: &Caller<'_>,
) -> Vec<&'a SessionEntry> {
    let active: Vec<&SessionEntry> = entries
        .iter()
        .filter(|e| e.status == "active" && caller.in_worktree(&e.data))
        .collect();
    if caller.agent_id.is_none()
        && active
            .iter()
            .any(|e| caller.relation(&e.data) == Relation::Foreign)
    {
        return Vec::new();
    }
    active
        .into_iter()
        .filter(|e| caller.relation(&e.data) != Relation::Foreign)
        .collect()
}

/// The `open` sessions `load_context` may activate on its own: the caller's
/// and the ownerless ones, never another agent's.
pub fn activatable_open<'a>(
    entries: &'a [SessionEntry],
    caller: &Caller<'_>,
) -> Vec<&'a SessionEntry> {
    entries
        .iter()
        .filter(|e| e.status == "open" && caller.relation(&e.data) != Relation::Foreign)
        .collect()
}

/// Sessions owned by another agent that the caller could take over, newest
/// first. Active and paused only (spec §4.4 rule 4).
pub fn adoptable<'a>(entries: &'a [SessionEntry], caller: &Caller<'_>) -> Vec<&'a SessionEntry> {
    let mut list: Vec<&SessionEntry> = entries
        .iter()
        .filter(|e| {
            matches!(e.status.as_str(), "active" | "paused")
                && caller.relation(&e.data) == Relation::Foreign
        })
        .collect();
    list.sort_by_key(|e| std::cmp::Reverse(updated_key(e)));
    list
}

/// `updated_at` for listings: the last save time, else the creation time
/// embedded in the id.
pub fn updated_at(s: &SessionData) -> Option<String> {
    s.ended_at.clone().or_else(|| {
        s.id.as_deref()
            .and_then(super::sessions::timestamp_from_id)
            .map(|t| t.to_rfc3339())
    })
}
