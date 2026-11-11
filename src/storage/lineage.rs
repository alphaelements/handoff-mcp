//! Which closed session a new session continues (FR-V0473-009, spec §4.4 d /
//! §4.4').
//!
//! A new session records the sessions it continues (`SessionData::continues`).
//! A *head* is a closed session no other closed session continues, i.e. the
//! latest of its lineage. [`decide`] picks the head the current work most
//! likely continues, or reports that it cannot tell. Everything in this module
//! is pure: git and agent facts are handed in, so the rules can be tested
//! without a repository.
//!
//! Decision rules (SPEC-V0473-209):
//! - candidates are heads that ended within [`CANDIDATE_WINDOW_DAYS`], newest
//!   first, at most [`CANDIDATE_MAX`];
//! - one candidate is confirmed outright (a score threshold made 14 of 21
//!   consecutive real sessions ambiguous), unless another agent's active
//!   session already continues it;
//! - several candidates are scored ([`score`]) and the best is confirmed only
//!   when it leads the runner-up by [`CONFIRM_MARGIN`] and no other agent's
//!   active session already continues it (an in-flight loser does not block);
//! - no candidate but some head: ambiguous and `stale`.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::session_target::effective_owner;
use super::sessions::{session_age, SessionData};

/// `SessionData::lineage_v` written by this build.
pub const LINEAGE_V: u32 = 1;
/// Heads older than this are not candidates for an automatic choice.
pub const CANDIDATE_WINDOW_DAYS: i64 = 30;
/// Most heads that are scored.
pub const CANDIDATE_MAX: usize = 8;
/// Most heads listed as `other_heads` when the choice is ambiguous.
pub const OTHER_HEADS_MAX: usize = 5;
/// The best score must beat the runner-up by at least this much.
pub const CONFIRM_MARGIN: i32 = 2;
/// A candidate that lists more related tasks than this gets no task-overlap
/// points: a long list overlaps with everything.
pub const RELATED_TASKS_MAX: usize = 20;
/// Most task ids a `t419-t424` range in a branch name expands to.
pub const TASK_RANGE_MAX: u32 = 10;
/// Commits of `HEAD` history fetched for the distance signal.
pub const HISTORY_DEPTH: usize = 500;

const POINTS_BRANCH: i32 = 3;
const POINTS_TASKS: i32 = 2;
const POINTS_PATHS: i32 = 1;
const POINTS_AGENT: i32 = 1;
const POINTS_WORKTREE: i32 = 1;
const COMMIT_NEAR: usize = 20;
const COMMIT_FAR: usize = 100;

/// A closed session no other closed session continues.
#[derive(Debug, Clone)]
pub struct Head<'a> {
    pub session: &'a SessionData,
    /// Ids of the active sessions that continue this head. Not empty when
    /// somebody is already working on top of it.
    pub in_flight_by: Vec<String>,
}

/// Compute the head set from the closed sessions and the active ones.
///
/// - New-format sessions (`lineage_v`) are heads unless a closed session
///   continues them, they carry `lineage_end`, or they are a worktree session
///   hanging off a parent (`parent_session_id` + `scope: worktree`).
/// - Old-format sessions form one chronological lineage: only the newest is a
///   head candidate, and it stops being one once a closed session continues it.
///
/// Reads nothing and writes nothing.
pub fn find_heads<'a>(closed: &'a [SessionData], actives: &[&SessionData]) -> Vec<Head<'a>> {
    // A worktree session hangs off its parent without superseding it, so what
    // it "continues" does not stop the parent from being the head.
    let continued: HashSet<&str> = closed
        .iter()
        .filter(|s| !is_worktree_child(s))
        .flat_map(|s| s.continues_ids().iter().map(String::as_str))
        .collect();
    let legacy_tip = closed
        .iter()
        .filter(|s| s.lineage_v.is_none())
        .max_by_key(|s| age_key(s))
        .and_then(|s| s.id.as_deref());

    closed
        .iter()
        .filter(|s| {
            let Some(id) = s.id.as_deref() else {
                return false;
            };
            if s.lineage_end == Some(true) || continued.contains(id) {
                return false;
            }
            if s.lineage_v.is_some() {
                !is_worktree_child(s)
            } else {
                legacy_tip == Some(id)
            }
        })
        .map(|s| {
            let id = s.id.as_deref().unwrap_or_default();
            let in_flight_by = actives
                .iter()
                .filter(|a| a.continues_ids().iter().any(|c| c == id))
                .filter_map(|a| a.id.clone())
                .collect();
            Head {
                session: s,
                in_flight_by,
            }
        })
        .collect()
}

/// A new-format session created in a linked worktree under a parent session.
fn is_worktree_child(s: &SessionData) -> bool {
    s.lineage_v.is_some() && s.parent_session_id.is_some() && s.scope.as_deref() == Some("worktree")
}

/// Ids of the heads: what pruning must never delete.
pub fn head_ids(closed: &[SessionData]) -> HashSet<String> {
    find_heads(closed, &[])
        .into_iter()
        .filter_map(|h| h.session.id.clone())
        .collect()
}

/// Age ordering shared with pruning (see `sessions::session_age`): sessions
/// with neither `ended_at` nor an id timestamp are the oldest.
fn age_key(s: &SessionData) -> (Option<DateTime<chrono::FixedOffset>>, String) {
    (session_age(s), s.id.clone().unwrap_or_default())
}

/// What the caller is: the identity-side facts the score and the evidence use.
#[derive(Debug, Clone, Default)]
pub struct Identity {
    pub agent_id: Option<String>,
    /// `worktree_key` of the caller's checkout (`None` outside git).
    pub worktree_key: Option<String>,
    /// Tasks the caller's agent has claimed.
    pub claimed: BTreeSet<String>,
}

/// What git says about the current checkout. Every field is empty when git
/// failed: the git-dependent signals then score 0 rather than erroring.
#[derive(Debug, Clone, Default)]
pub struct GitFacts {
    /// `None` on a detached `HEAD`.
    pub branch: Option<String>,
    /// Full commit hashes from `HEAD` backwards; the index is the distance.
    pub history: Vec<String>,
    /// Task ids named by the branch and the latest commit subjects.
    pub task_ids: BTreeSet<String>,
    /// Repo-relative paths of modified/untracked files.
    pub dirty_paths: BTreeSet<String>,
    /// Absolute repo root, used to turn absolute context-pointer paths
    /// repo-relative.
    pub root: Option<String>,
}

/// One scoring signal and the points it gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    pub name: &'static str,
    pub points: i32,
}

#[derive(Debug, Clone, Default)]
pub struct Score {
    pub total: i32,
    pub signals: Vec<Signal>,
    /// Signals that need git and were not evaluated.
    pub not_evaluated: Vec<&'static str>,
}

impl Score {
    fn push(&mut self, name: &'static str, points: i32) {
        self.total += points;
        self.signals.push(Signal { name, points });
    }

    fn signals_json(&self) -> Value {
        let map: BTreeMap<&str, i32> = self.signals.iter().map(|s| (s.name, s.points)).collect();
        json!(map)
    }
}

/// Score one candidate against the current situation (SPEC-V0473-309).
pub fn score(session: &SessionData, identity: &Identity, git: &GitFacts) -> Score {
    score_with(session, identity, Some(git))
}

/// [`score`] with the git signals optional: `None` evaluates only what needs
/// no git and lists the rest in [`Score::not_evaluated`].
fn score_with(session: &SessionData, identity: &Identity, git: Option<&GitFacts>) -> Score {
    let mut sc = Score::default();

    match git {
        Some(g) => {
            let branch_hit = matches!(
                (g.branch.as_deref(), session.branch.as_deref()),
                (Some(cur), Some(theirs)) if cur == theirs
            );
            sc.push("branch", if branch_hit { POINTS_BRANCH } else { 0 });
            sc.push("commit", commit_points(session, g));
        }
        None => sc.not_evaluated.extend(["branch", "commit"]),
    }

    let mut current_tasks: BTreeSet<&str> = identity.claimed.iter().map(String::as_str).collect();
    if let Some(g) = git {
        current_tasks.extend(g.task_ids.iter().map(String::as_str));
    }
    let overlap = session.related_task_ids.len() <= RELATED_TASKS_MAX
        && session
            .related_task_ids
            .iter()
            .any(|t| current_tasks.contains(t.as_str()));
    sc.push("task_overlap", if overlap { POINTS_TASKS } else { 0 });

    match git {
        Some(g) => {
            let hit = session
                .context_pointers
                .iter()
                .filter_map(|p| p.get("path").and_then(Value::as_str))
                .any(|p| g.dirty_paths.contains(&repo_relative(p, g.root.as_deref())));
            sc.push("context_pointers", if hit { POINTS_PATHS } else { 0 });
        }
        None => sc.not_evaluated.push("context_pointers"),
    }

    let agent_hit =
        identity.agent_id.is_some() && effective_owner(session) == identity.agent_id.as_deref();
    sc.push("agent_id", if agent_hit { POINTS_AGENT } else { 0 });
    let worktree_hit = identity.worktree_key.is_some()
        && session.worktree_key.as_deref() == identity.worktree_key.as_deref();
    sc.push("worktree", if worktree_hit { POINTS_WORKTREE } else { 0 });
    sc
}

/// +3 at `HEAD`, +2 within 20 commits, +1 within 100, else 0 (also when the
/// commit is not in the fetched history, e.g. after a rebase).
fn commit_points(session: &SessionData, git: &GitFacts) -> i32 {
    let Some(commit) = session
        .commit
        .as_deref()
        .filter(|c| c.len() >= 4 && c.bytes().all(|b| b.is_ascii_hexdigit()))
    else {
        return 0;
    };
    match git.history.iter().position(|h| h.starts_with(commit)) {
        Some(0) => 3,
        Some(d) if d <= COMMIT_NEAR => 2,
        Some(d) if d <= COMMIT_FAR => 1,
        _ => 0,
    }
}

fn repo_relative(path: &str, root: Option<&str>) -> String {
    let path = path.strip_prefix("./").unwrap_or(path);
    match root.and_then(|r| path.strip_prefix(r)) {
        Some(rest) => rest.trim_start_matches('/').to_string(),
        None => path.to_string(),
    }
}

/// A head together with its score, when it was scored.
#[derive(Debug, Clone)]
pub struct Ranked<'a> {
    pub head: &'a Head<'a>,
    pub score: Option<i32>,
}

#[derive(Debug)]
pub enum Decision<'a> {
    /// No closed session is a head (a fresh project).
    NoHeads,
    Confirmed {
        head: &'a Head<'a>,
        evidence: Value,
    },
    /// The server cannot tell. `ranked` is best first, at most
    /// [`OTHER_HEADS_MAX`]; `stale` means every head is older than the
    /// candidate window.
    Ambiguous {
        ranked: Vec<Ranked<'a>>,
        stale: bool,
    },
}

/// Choose among `heads`. `load_git` runs at most once and only when two or
/// more candidates need telling apart.
pub fn decide<'a>(
    heads: &'a [Head<'a>],
    now: DateTime<Utc>,
    identity: &Identity,
    load_git: impl FnOnce() -> GitFacts,
) -> Decision<'a> {
    if heads.is_empty() {
        return Decision::NoHeads;
    }
    let mut candidates: Vec<&Head<'_>> = heads
        .iter()
        .filter(|h| {
            session_age(h.session).is_some_and(|a| {
                now.signed_duration_since(a) <= chrono::Duration::days(CANDIDATE_WINDOW_DAYS)
            })
        })
        .collect();
    candidates.sort_by_key(|h| std::cmp::Reverse(age_key(h.session)));
    candidates.truncate(CANDIDATE_MAX);

    match candidates.as_slice() {
        [] => {
            let newest = heads
                .iter()
                .max_by_key(|h| age_key(h.session))
                .expect("heads is not empty");
            Decision::Ambiguous {
                ranked: vec![Ranked {
                    head: newest,
                    score: None,
                }],
                stale: true,
            }
        }
        [only] => {
            if !only.in_flight_by.is_empty() {
                return Decision::Ambiguous {
                    ranked: vec![Ranked {
                        head: only,
                        score: None,
                    }],
                    stale: false,
                };
            }
            let sc = score_with(only.session, identity, None);
            let evidence = json!({
                "candidates": 1,
                "signals": sc.signals_json(),
                "not_evaluated": sc.not_evaluated,
            });
            Decision::Confirmed {
                head: only,
                evidence,
            }
        }
        many => {
            let git = load_git();
            let mut scored: Vec<(&Head<'_>, Score)> = many
                .iter()
                .map(|h| (*h, score(h.session, identity, &git)))
                .collect();
            // Best score first; recency settles ties (candidates are already
            // newest first and the sort is stable).
            scored.sort_by_key(|(_, sc)| std::cmp::Reverse(sc.total));

            // An in-flight head only matters when it is the leader: a loser
            // (however close behind, the margin already covers that) is
            // somebody else's lineage and says nothing about ours.
            let lead = scored[0].1.total - scored[1].1.total;
            let leader_in_flight = !scored[0].0.in_flight_by.is_empty();
            if lead >= CONFIRM_MARGIN && !leader_in_flight {
                let (head, sc) = &scored[0];
                let evidence = json!({
                    "candidates": many.len(),
                    "score": sc.total,
                    "runner_up_score": scored[1].1.total,
                    "signals": sc.signals_json(),
                });
                return Decision::Confirmed { head, evidence };
            }
            Decision::Ambiguous {
                ranked: scored
                    .into_iter()
                    .take(OTHER_HEADS_MAX)
                    .map(|(head, sc)| Ranked {
                        head,
                        score: Some(sc.total),
                    })
                    .collect(),
                stale: false,
            }
        }
    }
}

/// Task ids (`t12`, `t439.6`) named in `text`, e.g. a branch name or commit
/// subject. A `t419-t424` range expands to its members, at most
/// [`TASK_RANGE_MAX`] of them.
pub fn extract_task_ids(text: &str) -> BTreeSet<String> {
    let bytes = text.as_bytes();
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b't' || (i > 0 && bytes[i - 1].is_ascii_alphanumeric()) {
            i += 1;
            continue;
        }
        let Some((end, id)) = task_id_at(bytes, i) else {
            i += 1;
            continue;
        };
        let top_level = !id.contains('.');
        if top_level {
            if let Some((range_end, last)) = range_tail(bytes, end) {
                let first: u32 = id[1..].parse().unwrap_or(0);
                if last >= first {
                    let stop = last.min(first.saturating_add(TASK_RANGE_MAX - 1));
                    out.extend((first..=stop).map(|n| format!("t{n}")));
                    i = range_end;
                    continue;
                }
            }
        }
        out.insert(id);
        i = end;
    }
    out
}

/// `t<digits>(.<digits>)*` starting at `start`, not followed by an
/// alphanumeric. Returns the end index and the id.
fn task_id_at(bytes: &[u8], start: usize) -> Option<(usize, String)> {
    let mut j = start + 1;
    let digits_from = j;
    while j < bytes.len() && bytes[j].is_ascii_digit() {
        j += 1;
    }
    if j == digits_from {
        return None;
    }
    while j + 1 < bytes.len() && bytes[j] == b'.' && bytes[j + 1].is_ascii_digit() {
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
    }
    if j < bytes.len() && bytes[j].is_ascii_alphanumeric() {
        return None;
    }
    Some((j, String::from_utf8_lossy(&bytes[start..j]).into_owned()))
}

/// `-t<digits>` right after a top-level id: the end of a range.
fn range_tail(bytes: &[u8], at: usize) -> Option<(usize, u32)> {
    if bytes.get(at) != Some(&b'-') || bytes.get(at + 1) != Some(&b't') {
        return None;
    }
    let (end, id) = task_id_at(bytes, at + 1)?;
    if id.contains('.') {
        return None;
    }
    Some((end, id[1..].parse().ok()?))
}

/// Repo-relative paths from `git status --porcelain` lines (`XY path`, with
/// `old -> new` for renames and quotes around unusual names).
pub fn dirty_paths_from_porcelain(lines: &[String]) -> BTreeSet<String> {
    lines
        .iter()
        .filter_map(|l| l.get(3..))
        .map(|rest| rest.rsplit(" -> ").next().unwrap_or(rest))
        .map(|p| p.trim().trim_matches('"').to_string())
        .filter(|p| !p.is_empty())
        .collect()
}
